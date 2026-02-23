//! Shared-memory backend primitives for Phase 7.
//!
//! This module provides a Linux `memfd` + `mmap(MAP_SHARED)` region with
//! coarse-grained `fcntl(F_SETLKW)` locking and a compact append-only entry
//! format. It is intentionally backend-focused and shell-hook integration is
//! layered on top in subsequent slices.

#[cfg(target_os = "linux")]
mod imp {
    use std::ffi::CString;
    use std::mem::size_of;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::ptr::NonNull;

    use crate::error;

    const SHARED_MAGIC: u32 = 0x4D43_5348; // "MCSH"
    const SHARED_VERSION: u32 = 1;

    const ENTRY_STATE_LIVE: u8 = 0x01;
    const ENTRY_STATE_TOMBSTONE: u8 = 0xFF;

    /// Entry type identifiers in the shared region.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(u8)]
    pub enum EntryType {
        /// Scalar variable value.
        Scalar = 1,
        /// Indexed array element.
        ArrayElement = 2,
        /// Associative array element.
        AssocElement = 3,
        /// Metadata entry (e.g. type tags).
        Meta = 4,
    }

    impl EntryType {
        fn from_byte(v: u8) -> Option<Self> {
            match v {
                1 => Some(Self::Scalar),
                2 => Some(Self::ArrayElement),
                3 => Some(Self::AssocElement),
                4 => Some(Self::Meta),
                _ => None,
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct RegionHeader {
        magic: u32,
        version: u32,
        total_size: u32,
        used_bytes: u32,
        entry_count: u32,
        tombstone_count: u32,
        generation: u64,
        reserved: [u8; 16],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct EntryHeader {
        state: u8,
        entry_type: u8,
        name_len: u16,
        key_len: u16,
        val_len: u32,
    }

    #[derive(Clone, Debug)]
    struct DecodedEntry {
        state: u8,
        entry_type: EntryType,
        name: String,
        val: Vec<u8>,
    }

    /// Shared-memory region handle.
    pub struct SharedRegion {
        fd: OwnedFd,
        ptr: NonNull<u8>,
        size: usize,
    }

    impl SharedRegion {
        /// Creates a new anonymous shared region with the given size.
        pub fn create(size: usize) -> Result<Self, error::Error> {
            if size < size_of::<RegionHeader>() + size_of::<EntryHeader>() {
                return Err(error::ErrorKind::InternalError(
                    "shared region size too small".to_string(),
                )
                .into());
            }

            let name = CString::new("brush-shared")
                .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
            let fd = memfd_create_cloexec(name.as_c_str())?;

            let truncate_rc = unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) };
            if truncate_rc != 0 {
                return Err(std::io::Error::last_os_error().into());
            }

            let mapped = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error().into());
            }

            let ptr = NonNull::new(mapped.cast::<u8>()).ok_or_else(|| {
                error::ErrorKind::InternalError("mmap returned null".to_string())
            })?;

            let mut region = Self { fd, ptr, size };
            region.initialize_header()?;
            Ok(region)
        }

        /// Sets a scalar value in the shared store.
        pub fn set_scalar(&mut self, name: &str, value: &str) -> Result<(), error::Error> {
            self.with_write_lock(|region| {
                let entry = EntryHeader {
                    state: ENTRY_STATE_LIVE,
                    entry_type: EntryType::Scalar as u8,
                    name_len: u16::try_from(name.len())
                        .map_err(|_| error::ErrorKind::InternalError("name too long".to_string()))?,
                    key_len: 0,
                    val_len: u32::try_from(value.len())
                        .map_err(|_| error::ErrorKind::InternalError("value too long".to_string()))?,
                };
                region.append_entry(&entry, name.as_bytes(), &[], value.as_bytes())?;
                Ok(())
            })
        }

        /// Reads the effective scalar value for `name`.
        pub fn get_scalar(&self, name: &str) -> Result<Option<String>, error::Error> {
            self.with_read_lock(|region| {
                let entries = region.scan_entries()?;
                let mut current: Option<String> = None;
                for e in entries {
                    if e.name != name {
                        continue;
                    }
                    if e.state == ENTRY_STATE_TOMBSTONE {
                        current = None;
                        continue;
                    }
                    if e.entry_type == EntryType::Scalar {
                        current = Some(String::from_utf8_lossy(&e.val).to_string());
                    }
                }
                Ok(current)
            })
        }

        /// Tombstones all values for `name`.
        pub fn unset_name(&mut self, name: &str) -> Result<(), error::Error> {
            self.with_write_lock(|region| {
                let entry = EntryHeader {
                    state: ENTRY_STATE_TOMBSTONE,
                    entry_type: EntryType::Meta as u8,
                    name_len: u16::try_from(name.len())
                        .map_err(|_| error::ErrorKind::InternalError("name too long".to_string()))?,
                    key_len: 0,
                    val_len: 0,
                };
                region.append_entry(&entry, name.as_bytes(), &[], &[])?;
                Ok(())
            })
        }

        fn initialize_header(&mut self) -> Result<(), error::Error> {
            self.with_write_lock(|region| {
                let mut header = region.read_header()?;
                if header.magic == SHARED_MAGIC {
                    return Ok(());
                }
                header = RegionHeader {
                    magic: SHARED_MAGIC,
                    version: SHARED_VERSION,
                    total_size: u32::try_from(region.size).map_err(|_| {
                        error::ErrorKind::InternalError("region too large".to_string())
                    })?,
                    used_bytes: u32::try_from(size_of::<RegionHeader>()).map_err(|_| {
                        error::ErrorKind::InternalError("header size overflow".to_string())
                    })?,
                    entry_count: 0,
                    tombstone_count: 0,
                    generation: 0,
                    reserved: [0; 16],
                };
                region.write_header(&header)
            })
        }

        fn with_read_lock<T>(
            &self,
            f: impl FnOnce(&Self) -> Result<T, error::Error>,
        ) -> Result<T, error::Error> {
            self.lock(libc::F_RDLCK as libc::c_short)?;
            let out = f(self);
            let _ = self.unlock();
            out
        }

        fn with_write_lock<T>(
            &mut self,
            f: impl FnOnce(&mut Self) -> Result<T, error::Error>,
        ) -> Result<T, error::Error> {
            self.lock(libc::F_WRLCK as libc::c_short)?;
            let out = f(self);
            let _ = self.unlock();
            out
        }

        fn lock(&self, lock_type: libc::c_short) -> Result<(), error::Error> {
            let mut fl = libc::flock {
                l_type: lock_type,
                l_whence: libc::SEEK_SET as libc::c_short,
                l_start: 0,
                l_len: 0,
                l_pid: 0,
            };
            let rc = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETLKW, &mut fl) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }

        fn unlock(&self) -> Result<(), error::Error> {
            let mut fl = libc::flock {
                l_type: libc::F_UNLCK as libc::c_short,
                l_whence: libc::SEEK_SET as libc::c_short,
                l_start: 0,
                l_len: 0,
                l_pid: 0,
            };
            let rc = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETLK, &mut fl) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }

        fn read_header(&self) -> Result<RegionHeader, error::Error> {
            if self.size < size_of::<RegionHeader>() {
                return Err(error::ErrorKind::InternalError("region too small".to_string()).into());
            }
            let p = self.ptr.as_ptr().cast::<RegionHeader>();
            let header = unsafe { std::ptr::read_unaligned(p) };
            Ok(header)
        }

        fn write_header(&mut self, header: &RegionHeader) -> Result<(), error::Error> {
            let p = self.ptr.as_ptr().cast::<RegionHeader>();
            unsafe { std::ptr::write_unaligned(p, *header) };
            Ok(())
        }

        fn append_entry(
            &mut self,
            entry: &EntryHeader,
            name: &[u8],
            key: &[u8],
            val: &[u8],
        ) -> Result<(), error::Error> {
            let mut header = self.read_header()?;
            let payload_len = name.len() + key.len() + val.len();
            let total_len = size_of::<EntryHeader>() + payload_len;
            let used = usize::try_from(header.used_bytes)
                .map_err(|_| error::ErrorKind::InternalError("used overflow".to_string()))?;
            if used + total_len > self.size {
                return Err(error::ErrorKind::InternalError("shared region full".to_string()).into());
            }

            let mut off = used;
            unsafe {
                std::ptr::write_unaligned(self.ptr.as_ptr().add(off).cast::<EntryHeader>(), *entry);
            }
            off += size_of::<EntryHeader>();
            unsafe {
                std::ptr::copy_nonoverlapping(name.as_ptr(), self.ptr.as_ptr().add(off), name.len());
            }
            off += name.len();
            unsafe {
                std::ptr::copy_nonoverlapping(key.as_ptr(), self.ptr.as_ptr().add(off), key.len());
            }
            off += key.len();
            unsafe {
                std::ptr::copy_nonoverlapping(val.as_ptr(), self.ptr.as_ptr().add(off), val.len());
            }

            header.used_bytes = u32::try_from(used + total_len)
                .map_err(|_| error::ErrorKind::InternalError("used overflow".to_string()))?;
            header.entry_count = header.entry_count.saturating_add(1);
            if entry.state == ENTRY_STATE_TOMBSTONE {
                header.tombstone_count = header.tombstone_count.saturating_add(1);
            }
            header.generation = header.generation.saturating_add(1);
            self.write_header(&header)
        }

        fn scan_entries(&self) -> Result<Vec<DecodedEntry>, error::Error> {
            let header = self.read_header()?;
            let used = usize::try_from(header.used_bytes)
                .map_err(|_| error::ErrorKind::InternalError("used overflow".to_string()))?;
            if used > self.size || used < size_of::<RegionHeader>() {
                return Err(error::ErrorKind::InternalError("corrupt shared header".to_string()).into());
            }

            let mut entries = Vec::new();
            let mut off = size_of::<RegionHeader>();
            while off + size_of::<EntryHeader>() <= used {
                let e = unsafe {
                    std::ptr::read_unaligned(self.ptr.as_ptr().add(off).cast::<EntryHeader>())
                };
                off += size_of::<EntryHeader>();

                let nlen = usize::from(e.name_len);
                let klen = usize::from(e.key_len);
                let vlen = usize::try_from(e.val_len)
                    .map_err(|_| error::ErrorKind::InternalError("entry overflow".to_string()))?;
                if off + nlen + klen + vlen > used {
                    return Err(error::ErrorKind::InternalError("corrupt shared entry".to_string()).into());
                }

                let name = unsafe {
                    let p = self.ptr.as_ptr().add(off);
                    std::slice::from_raw_parts(p, nlen)
                };
                off += nlen;
                let _key = unsafe {
                    let p = self.ptr.as_ptr().add(off);
                    std::slice::from_raw_parts(p, klen)
                };
                off += klen;
                let val = unsafe {
                    let p = self.ptr.as_ptr().add(off);
                    std::slice::from_raw_parts(p, vlen)
                };
                off += vlen;

                let Some(entry_type) = EntryType::from_byte(e.entry_type) else {
                    return Err(
                        error::ErrorKind::InternalError("unknown shared entry type".to_string())
                            .into(),
                    );
                };

                entries.push(DecodedEntry {
                    state: e.state,
                    entry_type,
                    name: String::from_utf8_lossy(name).to_string(),
                    val: val.to_vec(),
                });
            }

            Ok(entries)
        }
    }

    impl Drop for SharedRegion {
        fn drop(&mut self) {
            let _ = unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.size) };
        }
    }

    fn memfd_create_cloexec(name: &std::ffi::CStr) -> Result<OwnedFd, error::Error> {
        let flags = libc::MFD_CLOEXEC;
        let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags) as RawFd };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(owned)
    }

    #[cfg(test)]
    mod tests {
        use super::SharedRegion;

        #[test]
        fn scalar_roundtrip() -> anyhow::Result<()> {
            let mut region = SharedRegion::create(1024 * 1024)?;
            region.set_scalar("x", "1")?;
            assert_eq!(region.get_scalar("x")?.as_deref(), Some("1"));
            region.set_scalar("x", "42")?;
            assert_eq!(region.get_scalar("x")?.as_deref(), Some("42"));
            region.unset_name("x")?;
            assert_eq!(region.get_scalar("x")?, None);
            Ok(())
        }

        #[test]
        fn fork_visibility() -> anyhow::Result<()> {
            let mut region = SharedRegion::create(1024 * 1024)?;
            region.set_scalar("k", "parent")?;

            match unsafe { nix::unistd::fork()? } {
                nix::unistd::ForkResult::Child => {
                    // Child sees parent update and can write back.
                    if region.get_scalar("k")?.as_deref() != Some("parent") {
                        std::process::exit(2);
                    }
                    region.set_scalar("k", "child")?;
                    std::process::exit(0);
                }
                nix::unistd::ForkResult::Parent { child } => {
                    let status = nix::sys::wait::waitpid(child, None)?;
                    if !matches!(status, nix::sys::wait::WaitStatus::Exited(_, 0)) {
                        return Err(anyhow::anyhow!("child failed: {status:?}"));
                    }
                    assert_eq!(region.get_scalar("k")?.as_deref(), Some("child"));
                }
            }

            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::{EntryType, SharedRegion};

#[cfg(not(target_os = "linux"))]
mod imp {
    use crate::error;

    /// Entry type identifiers in the shared region.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(u8)]
    pub enum EntryType {
        /// Scalar variable value.
        Scalar = 1,
        /// Indexed array element.
        ArrayElement = 2,
        /// Associative array element.
        AssocElement = 3,
        /// Metadata entry (e.g. type tags).
        Meta = 4,
    }

    /// Unsupported platform placeholder.
    pub struct SharedRegion;

    impl SharedRegion {
        /// Unsupported on this platform.
        pub fn create(_size: usize) -> Result<Self, error::Error> {
            error::unimp("shared memory backend currently supports Linux only")
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub use imp::{EntryType, SharedRegion};

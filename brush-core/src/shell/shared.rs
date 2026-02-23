//! Shared variable support for shell.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::{ShellValue, ShellVariable, error, variables};

const DEFAULT_SHARED_REGION_SIZE: usize = 4096;

#[derive(Clone, Default)]
pub(crate) struct SharedContext {
    pub(crate) region: Option<Arc<Mutex<crate::shared_memory::SharedRegion>>>,
    pub(crate) bound_names: HashSet<String>,
}

impl SharedContext {
    fn ensure_region(
        &mut self,
    ) -> Result<Arc<Mutex<crate::shared_memory::SharedRegion>>, error::Error> {
        if let Some(region) = &self.region {
            return Ok(region.clone());
        }

        let region = crate::shared_memory::SharedRegion::create(DEFAULT_SHARED_REGION_SIZE)?;
        let region = Arc::new(Mutex::new(region));
        self.region = Some(region.clone());
        Ok(region)
    }
}

impl<SE: crate::extensions::ShellExtensions> crate::Shell<SE> {
    /// Returns true if `name` is currently bound to shared storage.
    pub fn shared_is_bound(&self, name: &str) -> bool {
        self.shared.bound_names.contains(name)
    }

    /// Binds a scalar shell variable to shared storage, optionally setting an initial value.
    pub fn shared_bind_scalar(
        &mut self,
        name: &str,
        initial_value: Option<&str>,
    ) -> Result<(), error::Error> {
        let initial_value = if let Some(v) = initial_value {
            v.to_owned()
        } else if let Some((_, var)) = self.env().get(name) {
            var.value().to_cow_str(self).into_owned()
        } else {
            String::new()
        };

        let region = self.shared.ensure_region()?;
        let mut region = region.lock().map_err(|_| {
            error::Error::from(error::ErrorKind::InternalError(
                "shared region lock poisoned".to_string(),
            ))
        })?;
        region.set_scalar(name, initial_value.as_str())?;

        self.shared.bound_names.insert(name.to_owned());
        self.env_mut().update_or_add(
            name,
            variables::ShellValueLiteral::Scalar(initial_value),
            |_| Ok(()),
            crate::env::EnvironmentLookup::Anywhere,
            crate::env::EnvironmentScope::Global,
        )?;

        Ok(())
    }

    /// Binds an indexed array variable to shared storage.
    pub fn shared_bind_indexed_array(&mut self, name: &str) -> Result<(), error::Error> {
        let mut arr = std::collections::BTreeMap::new();
        if let Some((_, var)) = self.env().get(name) {
            match var.value() {
                ShellValue::IndexedArray(values) => {
                    arr = values.clone();
                }
                ShellValue::Unset(crate::variables::ShellValueUnsetType::IndexedArray) => {}
                _ => {}
            }
        }

        let region = self.shared.ensure_region()?;
        let mut region = region.lock().map_err(|_| {
            error::Error::from(error::ErrorKind::InternalError(
                "shared region lock poisoned".to_string(),
            ))
        })?;
        region.set_indexed_array(name, &arr)?;

        self.shared.bound_names.insert(name.to_owned());
        self.env_mut()
            .set_global(name, ShellVariable::new(ShellValue::IndexedArray(arr)))?;
        Ok(())
    }

    /// Binds an associative array variable to shared storage.
    pub fn shared_bind_assoc_array(&mut self, name: &str) -> Result<(), error::Error> {
        let mut arr = std::collections::BTreeMap::new();
        if let Some((_, var)) = self.env().get(name) {
            match var.value() {
                ShellValue::AssociativeArray(values) => {
                    arr = values.clone();
                }
                ShellValue::Unset(crate::variables::ShellValueUnsetType::AssociativeArray) => {}
                _ => {}
            }
        }

        let region = self.shared.ensure_region()?;
        let mut region = region.lock().map_err(|_| {
            error::Error::from(error::ErrorKind::InternalError(
                "shared region lock poisoned".to_string(),
            ))
        })?;
        region.set_assoc_array(name, &arr)?;

        self.shared.bound_names.insert(name.to_owned());
        self.env_mut()
            .set_global(name, ShellVariable::new(ShellValue::AssociativeArray(arr)))?;
        Ok(())
    }

    /// Binds an integer variable to shared storage, optionally setting an initial value.
    pub fn shared_bind_integer(
        &mut self,
        name: &str,
        initial_value: Option<&str>,
    ) -> Result<(), error::Error> {
        let value = if let Some(v) = initial_value {
            v.to_owned()
        } else if let Some((_, var)) = self.env().get(name) {
            var.value().to_cow_str(self).into_owned()
        } else {
            "0".to_string()
        };

        let region = self.shared.ensure_region()?;
        let mut region = region.lock().map_err(|_| {
            error::Error::from(error::ErrorKind::InternalError(
                "shared region lock poisoned".to_string(),
            ))
        })?;
        region.set_scalar(name, value.as_str())?;
        region.set_meta(name, "type", "integer")?;

        self.shared.bound_names.insert(name.to_owned());
        let mut var = ShellVariable::new(value);
        var.treat_as_integer();
        self.env_mut().set_global(name, var)?;
        Ok(())
    }

    /// Deletes a shared variable binding and removes its backing shared value.
    pub fn shared_delete(&mut self, name: &str) -> Result<bool, error::Error> {
        if !self.shared.bound_names.contains(name) {
            return Ok(false);
        }

        if let Some(region) = &self.shared.region {
            let mut region = region.lock().map_err(|_| {
                error::Error::from(error::ErrorKind::InternalError(
                    "shared region lock poisoned".to_string(),
                ))
            })?;
            region.unset_name(name)?;
        }

        self.shared.bound_names.remove(name);
        let _ = self.env_mut().unset(name)?;
        Ok(true)
    }

    /// Unsets a variable in the shell environment and shared backing (if bound).
    pub fn shared_unset_var(&mut self, name: &str) -> Result<Option<ShellVariable>, error::Error> {
        if self.shared.bound_names.contains(name)
            && let Some(region) = &self.shared.region
        {
            let mut region = region.lock().map_err(|_| {
                error::Error::from(error::ErrorKind::InternalError(
                    "shared region lock poisoned".to_string(),
                ))
            })?;
            region.unset_name(name)?;
        }

        self.env_mut().unset(name)
    }

    /// Pushes the shell variable's current value into shared storage when the name is bound.
    pub fn shared_sync_from_env(&mut self, name: &str) -> Result<(), error::Error> {
        if !self.shared.bound_names.contains(name) {
            return Ok(());
        }

        let var = self.env().get(name).map(|(_, var)| var.clone());
        let region = self.shared.ensure_region()?;
        let mut region = region.lock().map_err(|_| {
            error::Error::from(error::ErrorKind::InternalError(
                "shared region lock poisoned".to_string(),
            ))
        })?;

        match var {
            Some(var) => match var.value() {
                ShellValue::String(s) => {
                    region.set_scalar(name, s.as_str())?;
                    if var.is_treated_as_integer() {
                        region.set_meta(name, "type", "integer")?;
                    }
                }
                ShellValue::IndexedArray(values) => {
                    region.set_indexed_array(name, values)?;
                }
                ShellValue::AssociativeArray(values) => {
                    region.set_assoc_array(name, values)?;
                }
                ShellValue::Unset(_) => {
                    region.unset_name(name)?;
                }
                ShellValue::Dynamic { .. } => {
                    return error::unimp("shared sync for dynamic values is not supported");
                }
            },
            None => {
                region.unset_name(name)?;
            }
        }

        Ok(())
    }

    /// Reads a variable as a cloned value, resolving shared-backed bindings from shared memory.
    pub fn env_var_cloned(&self, name: &str) -> Result<Option<ShellVariable>, error::Error> {
        if self.shared.bound_names.contains(name)
            && let Some(region) = &self.shared.region
        {
            let mut region = region.lock().map_err(|_| {
                error::Error::from(error::ErrorKind::InternalError(
                    "shared region lock poisoned".to_string(),
                ))
            })?;
            let var = match region.get_meta(name, "type")?.as_deref() {
                Some("array") => Some(ShellVariable::new(ShellValue::IndexedArray(
                    region.get_indexed_array(name)?,
                ))),
                Some("assoc") => Some(ShellVariable::new(ShellValue::AssociativeArray(
                    region.get_assoc_array(name)?,
                ))),
                Some("integer") => {
                    let mut var = ShellVariable::new(
                        region.get_scalar(name)?.unwrap_or_else(|| "0".to_string()),
                    );
                    var.treat_as_integer();
                    Some(var)
                }
                _ => region.get_scalar(name)?.map(ShellVariable::new),
            };
            return Ok(var);
        }

        Ok(self.env().get(name).map(|(_, var)| var.clone()))
    }
}

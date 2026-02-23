Name:           brush-shell
Version:        %{?version_override}%{!?version_override:0.3.0}
Release:        1%{?dist}
Summary:        Rust shell focused on POSIX and bash compatibility

License:        MIT
URL:            https://github.com/reubeno/brush
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc

%description
brush is a Rust-implemented shell focused on POSIX and bash compatibility.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --locked -p brush-shell %{?cargo_features}

%install
install -Dpm0755 target/release/brush %{buildroot}%{_bindir}/brush
install -Dpm0644 LICENSE %{buildroot}%{_licensedir}/%{name}/LICENSE
install -Dpm0644 README.md %{buildroot}%{_docdir}/%{name}/README.md
install -Dpm0644 CHANGELOG.md %{buildroot}%{_docdir}/%{name}/CHANGELOG.md

%files
%{_bindir}/brush
%license %{_licensedir}/%{name}/LICENSE
%doc %{_docdir}/%{name}/README.md
%doc %{_docdir}/%{name}/CHANGELOG.md

%changelog
* Mon Feb 23 2026 brush maintainers <maintainers@invalid> - %{version}-1
- Initial SRPM packaging target for brush-shell.

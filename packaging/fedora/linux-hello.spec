Name:           linux-hello
Version:        0.1.0
Release:        1%{?dist}
Summary:        Windows Hello-style facial authentication for Linux using OpenCV and PAM

License:        GPL-3.0-or-later
URL:            https://github.com/kennycoder/linux-hello
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc-c++
BuildRequires:  make
BuildRequires:  pkgconfig
BuildRequires:  opencv-devel
BuildRequires:  pam-devel
BuildRequires:  polkit-devel
BuildRequires:  clang-devel
BuildRequires:  cmake
BuildRequires:  extra-cmake-modules
BuildRequires:  kf6-kcoreaddons-devel

Requires:       opencv
Requires:       pam
Requires:       authselect
Requires:       polkit
Requires:       hicolor-icon-theme

%description
Linux Hello brings facial recognition authentication to Linux desktops using
standard webcams and infrared cameras, providing a Windows Hello-like
experience for sudo, login managers, and lock screens.

%prep
%autosetup -n %{name}-%{version}

%build
%make_build build

%install
%make_install PREFIX=/usr

%post
%systemd_post linux-hello.service

%preun
%systemd_preun linux-hello.service
if [ $1 -eq 0 ]; then
    # Package removal: rollback PAM changes if configured via authselect
    if [ -x %{_bindir}/linux-hello ]; then
        %{_bindir}/linux-hello disable 2>/dev/null || true
    fi
fi

%postun
%systemd_postun_with_restart linux-hello.service

%files
%license LICENSE
%doc README.md
%{_bindir}/linux-hello
%{_bindir}/linux-hellod
%{_bindir}/linux-hello-gui
%{_libdir}/security/pam_linux_hello.so
%{_datadir}/applications/linux-hello-gui.desktop
%{_datadir}/icons/hicolor/scalable/apps/linux-hello.svg
%{_datadir}/polkit-1/actions/org.linuxhello.pkexec.policy
%{_datadir}/linux-hello/
/lib/systemd/system/linux-hello.service
%dir /var/lib/linux-hello
%dir /var/lib/linux-hello/models

%changelog
* Sun Oct 25 2026 Nikolai <kennycoder> - 0.1.0-1
- Initial RPM release


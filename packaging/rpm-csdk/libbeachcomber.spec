Name:           libbeachcomber-devel
Version:        %{version}
Release:        1%{?dist}
Summary:        C client library for the beachcomber daemon (development files)
License:        MIT
URL:            https://github.com/NavistAu/beachcomber
Source0:        https://github.com/NavistAu/beachcomber/archive/refs/tags/v%{version}.tar.gz#/beachcomber-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  make

%description
Header and pkg-config file for libbeachcomber — the C ABI of the
beachcomber daemon's shared client library. The shared library itself
ships with the beachcomber daemon package.

%prep
%autosetup -n beachcomber-%{version}

%build
# Nothing to compile: the package ships the generated header and a
# pkg-config file. The shared library ships with the daemon package.
true

%install
%make_install -C sdks/c PREFIX=/usr VERSION=%{version}

%files
%license LICENSE
/usr/include/beachcomber.h
/usr/lib/pkgconfig/libbeachcomber.pc

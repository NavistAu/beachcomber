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
Headers, shared library, static library, and pkg-config file for
libbeachcomber — a C client library for the beachcomber daemon.

%prep
%autosetup -n beachcomber-%{version}

%build
%make_build -C sdks/c VERSION=%{version}

%install
%make_install -C sdks/c PREFIX=/usr VERSION=%{version}

%files
%license LICENSE
/usr/include/beachcomber.h
/usr/include/json.h
/usr/lib/libbeachcomber.so
/usr/lib/libbeachcomber.a
/usr/lib/pkgconfig/libbeachcomber.pc

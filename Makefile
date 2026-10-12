PREFIX ?= /usr
DESTDIR ?=

CARGO ?= cargo
CARGO_FLAGS ?= --release

# When running via sudo, inherit the invoking user's CARGO_HOME if root doesn't have one
ifeq ($(CARGO_HOME),)
  ifneq ($(SUDO_USER),)
    SUDO_USER_HOME := $(shell getent passwd $(SUDO_USER) | cut -d: -f6)
    ifneq ($(wildcard $(SUDO_USER_HOME)/.cargo),)
      export CARGO_HOME := $(SUDO_USER_HOME)/.cargo
    endif
  endif
endif

all: build

build:
	$(CARGO) build $(CARGO_FLAGS)

test:
	$(CARGO) test $(CARGO_FLAGS)

install:
	@if [ ! -f target/release/linux-hello ] || [ ! -f target/release/linux-hellod ] || [ ! -f target/release/libpam_linux_hello.so ]; then \
		echo "Release binaries not found. Building with $(CARGO)..."; \
		$(CARGO) build $(CARGO_FLAGS); \
	fi
	# Install binaries
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 target/release/linux-hello $(DESTDIR)$(PREFIX)/bin/linux-hello
	install -m 755 target/release/linux-hellod $(DESTDIR)$(PREFIX)/bin/linux-hellod

	# Install PAM module
	install -d $(DESTDIR)$(PREFIX)/lib/security
	install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)$(PREFIX)/lib/security/pam_linux_hello.so
	# Also link to multiarch location if present
	@if [ -d /lib/x86_64-linux-gnu/security ]; then \
		install -d $(DESTDIR)/lib/x86_64-linux-gnu/security; \
		install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)/lib/x86_64-linux-gnu/security/pam_linux_hello.so; \
	fi

	# Install ONNX models
	install -d $(DESTDIR)$(PREFIX)/share/linux-hello/models
	install -m 644 models/*.onnx $(DESTDIR)$(PREFIX)/share/linux-hello/models/

	# Storage directory
	install -d -m 755 $(DESTDIR)/var/lib/linux-hello/models

	# Systemd service
	install -d $(DESTDIR)/lib/systemd/system
	install -m 644 debian/linux-hello.service $(DESTDIR)/lib/systemd/system/linux-hello.service

	# Debian PAM profile
	install -d $(DESTDIR)$(PREFIX)/share/pam-configs
	install -m 644 debian/pam-config $(DESTDIR)$(PREFIX)/share/pam-configs/linux-hello

VERSION ?= 0.1.0
ARCH ?= amd64
DEB_BUILD_DIR = target/debian/linux-hello_$(VERSION)_$(ARCH)

deb: build
	rm -rf target/debian
	mkdir -p $(DEB_BUILD_DIR)/DEBIAN
	cp debian/control debian/postinst debian/prerm debian/postrm $(DEB_BUILD_DIR)/DEBIAN/
	chmod 755 $(DEB_BUILD_DIR)/DEBIAN
	chmod 755 $(DEB_BUILD_DIR)/DEBIAN/postinst $(DEB_BUILD_DIR)/DEBIAN/prerm $(DEB_BUILD_DIR)/DEBIAN/postrm
	$(MAKE) install DESTDIR=$(DEB_BUILD_DIR) PREFIX=/usr
	dpkg-deb --build --root-owner-group $(DEB_BUILD_DIR) target/linux-hello_$(VERSION)_$(ARCH).deb
	@echo "\n✓ Successfully built package: target/linux-hello_$(VERSION)_$(ARCH).deb"

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/linux-hello
	rm -f $(DESTDIR)$(PREFIX)/bin/linux-hellod
	rm -f $(DESTDIR)$(PREFIX)/lib/security/pam_linux_hello.so
	rm -f $(DESTDIR)/lib/x86_64-linux-gnu/security/pam_linux_hello.so
	rm -rf $(DESTDIR)$(PREFIX)/share/linux-hello
	rm -f $(DESTDIR)/lib/systemd/system/linux-hello.service
	rm -f $(DESTDIR)/etc/systemd/system/linux-hello.service
	rm -f $(DESTDIR)$(PREFIX)/share/pam-configs/linux-hello
	@if command -v pam-auth-update >/dev/null 2>&1; then \
		pam-auth-update --package; \
	fi

.PHONY: all build test install uninstall deb

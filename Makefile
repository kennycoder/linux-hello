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

check-deps:
	@command -v $(CARGO) >/dev/null 2>&1 || { \
		echo "Error: '$(CARGO)' not found. Install Rust toolchain via https://rustup.rs/"; \
		exit 1; \
	}
	@command -v c++ >/dev/null 2>&1 || { \
		echo "Error: C++ compiler ('c++') not found."; \
		echo "  Debian/Ubuntu: sudo apt install build-essential"; \
		echo "  RHEL/Fedora:   sudo dnf install gcc-c++ make"; \
		echo "  Arch Linux:    sudo pacman -S base-devel"; \
		exit 1; \
	}
	@if [ ! -f /usr/include/opencv4/opencv2/opencv.hpp ] && [ ! -f /usr/include/opencv5/opencv2/opencv.hpp ] && [ ! -f /usr/include/opencv/opencv2/opencv.hpp ] && ! (command -v pkg-config >/dev/null 2>&1 && (pkg-config --exists opencv4 || pkg-config --exists opencv5 || pkg-config --exists opencv)); then \
		echo "Error: OpenCV development headers not found."; \
		echo "  Debian/Ubuntu: sudo apt install libopencv-dev"; \
		echo "  RHEL/Fedora:   sudo dnf install opencv-devel"; \
		echo "  Arch Linux:    sudo pacman -S opencv"; \
		exit 1; \
	fi

build-kcm:
	@if command -v cmake >/dev/null 2>&1; then \
		echo "Configuring and building KDE Control Module (kcm_linuxhello)..."; \
		if cmake -B kde-kcm/build -S kde-kcm -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$(PREFIX) 2>/dev/null; then \
			cmake --build kde-kcm/build || { echo "Warning: KDE KCM build failed."; }; \
		else \
			echo "KDE build dependencies (ECM / KF6 / Qt6) not fully present; skipping KCM build."; \
		fi \
	fi

build: check-deps build-kcm
	$(CARGO) build $(CARGO_FLAGS)

test: check-deps
	$(CARGO) test $(CARGO_FLAGS)

clean:
	$(CARGO) clean
	rm -rf kde-kcm/build

install:
	@if [ ! -f target/release/linux-hello ] || [ ! -f target/release/linux-hellod ] || [ ! -f target/release/linux-hello-gui ] || [ ! -f target/release/libpam_linux_hello.so ]; then \
		echo "Release binaries not found. Building with $(CARGO)..."; \
		$(MAKE) build; \
	fi
	# Install binaries
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 target/release/linux-hello $(DESTDIR)$(PREFIX)/bin/linux-hello
	install -m 755 target/release/linux-hellod $(DESTDIR)$(PREFIX)/bin/linux-hellod
	install -m 755 target/release/linux-hello-gui $(DESTDIR)$(PREFIX)/bin/linux-hello-gui

	# Install desktop launcher, icon, and Polkit policy
	install -d $(DESTDIR)$(PREFIX)/share/applications
	install -m 644 debian/linux-hello-gui.desktop $(DESTDIR)$(PREFIX)/share/applications/linux-hello-gui.desktop
	install -d $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps
	install -m 644 assets/linux-hello-logo-cropped.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/linux-hello.svg
	install -d $(DESTDIR)$(PREFIX)/share/polkit-1/actions
	install -m 644 debian/org.linuxhello.pkexec.policy $(DESTDIR)$(PREFIX)/share/polkit-1/actions/org.linuxhello.pkexec.policy

	# Install PAM module
	install -d $(DESTDIR)$(PREFIX)/lib/security
	install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)$(PREFIX)/lib/security/pam_linux_hello.so
	# Also install to Debian multiarch or RHEL/Fedora lib64 PAM locations if present on host
	@if [ -d /lib/x86_64-linux-gnu/security ]; then \
		install -d $(DESTDIR)/lib/x86_64-linux-gnu/security; \
		install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)/lib/x86_64-linux-gnu/security/pam_linux_hello.so; \
	fi
	@if [ -d /usr/lib64/security ]; then \
		install -d $(DESTDIR)/usr/lib64/security; \
		install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)/usr/lib64/security/pam_linux_hello.so; \
	elif [ -d /lib/x86_64-linux-gnu/security ]; then \
		install -d $(DESTDIR)/lib/x86_64-linux-gnu/security; \
		install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)/lib/x86_64-linux-gnu/security/pam_linux_hello.so; \
	else \
		install -d $(DESTDIR)$(PREFIX)/lib/security; \
		install -m 644 target/release/libpam_linux_hello.so $(DESTDIR)$(PREFIX)/lib/security/pam_linux_hello.so; \
	fi

	# Install ONNX models
	install -d $(DESTDIR)$(PREFIX)/share/linux-hello/models
	install -m 644 models/*.onnx $(DESTDIR)$(PREFIX)/share/linux-hello/models/

	# Storage directory
	install -d -m 755 $(DESTDIR)/var/lib/linux-hello/models

	# Systemd service
	install -d $(DESTDIR)/lib/systemd/system
	install -m 644 debian/linux-hello.service $(DESTDIR)/lib/systemd/system/linux-hello.service

	# PAM profile configuration
	@if command -v authselect >/dev/null 2>&1; then \
		echo "Detected authselect (Fedora/RHEL)."; \
		if [ -z "$(DESTDIR)" ]; then \
			echo "Running '$(PREFIX)/bin/linux-hello enable' to activate PAM profile..."; \
			$(DESTDIR)$(PREFIX)/bin/linux-hello enable || true; \
		else \
			echo "Staging build (DESTDIR set); skipping live authselect configuration."; \
		fi; \
	elif [ -d /usr/share/pam-configs ] || command -v pam-auth-update >/dev/null 2>&1; then \
		echo "Detected Debian/Ubuntu PAM configuration framework."; \
		install -d $(DESTDIR)$(PREFIX)/share/pam-configs; \
		install -m 644 debian/pam-config $(DESTDIR)$(PREFIX)/share/pam-configs/linux-hello; \
	else \
		echo "Notice: Neither authselect nor pam-auth-update detected. Sudo-Only mode (linux-hello enable --sudo-only) or manual PAM entry is recommended."; \
	fi

	# Install KDE Plasma Control Module if built
	@if [ -f kde-kcm/build/bin/plasma/kcms/systemsettings/kcm_linuxhello.so ]; then \
		echo "Installing KDE Plasma Control Module..."; \
		DESTDIR=$(DESTDIR) cmake --install kde-kcm/build; \
	fi

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
	rm -f $(DESTDIR)$(PREFIX)/bin/linux-hello-gui
	rm -f $(DESTDIR)$(PREFIX)/share/applications/linux-hello-gui.desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/linux-hello.svg
	rm -f $(DESTDIR)$(PREFIX)/share/polkit-1/actions/org.linuxhello.pkexec.policy
	rm -f $(DESTDIR)$(PREFIX)/lib/security/pam_linux_hello.so
	rm -f $(DESTDIR)/lib/x86_64-linux-gnu/security/pam_linux_hello.so
	rm -f $(DESTDIR)/usr/lib64/security/pam_linux_hello.so
	rm -rf $(DESTDIR)$(PREFIX)/share/linux-hello
	rm -f $(DESTDIR)/lib/systemd/system/linux-hello.service
	rm -f $(DESTDIR)/etc/systemd/system/linux-hello.service
	rm -f $(DESTDIR)$(PREFIX)/share/pam-configs/linux-hello
	rm -f $(DESTDIR)$(PREFIX)/lib/x86_64-linux-gnu/qt6/plugins/plasma/kcms/systemsettings/kcm_linuxhello.so
	rm -f $(DESTDIR)$(PREFIX)/lib64/qt6/plugins/plasma/kcms/systemsettings/kcm_linuxhello.so
	rm -f $(DESTDIR)$(PREFIX)/share/applications/kcm_linuxhello.desktop
	@if [ -d /etc/authselect/custom/linux-hello ] || command -v authselect >/dev/null 2>&1; then \
		if [ -z "$(DESTDIR)" ] && [ -x $(PREFIX)/bin/linux-hello ]; then \
			$(PREFIX)/bin/linux-hello disable || true; \
		fi; \
	fi
	@if command -v pam-auth-update >/dev/null 2>&1; then \
		pam-auth-update --package; \
	fi


.PHONY: all check-deps build build-kcm test clean install uninstall deb


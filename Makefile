prefix ?= /usr/local
bindir = $(prefix)/bin
libdir = $(prefix)/lib
libexecdir = $(prefix)/libexec
includedir = $(prefix)/include
datarootdir = $(prefix)/share
datadir = $(datarootdir)
iconsdir = $(datarootdir)/icons/hicolor

TARGET = debug
DEBUG ?= 0
ifeq ($(DEBUG),0)
	TARGET = release
	ARGS += --release
endif

VENDOR ?= 0
ifneq ($(VENDOR),0)
	ARGS += --frozen
endif

BIN = xdg-desktop-portal-cosmic
DBUS_NAME = org.freedesktop.impl.portal.desktop.cosmic

all: $(BIN)

clean:
	rm -rf target

distclean: clean
	rm -rf .cargo vendor vendor.tar

$(BIN): Cargo.toml Cargo.lock src/main.rs vendor-check
	cargo build $(ARGS) --bin ${BIN}

install:
	install -Dm0755 target/$(TARGET)/$(BIN) $(DESTDIR)$(libexecdir)/$(BIN)
	install -Dm0644 data/$(DBUS_NAME).service $(DESTDIR)$(datadir)/dbus-1/services/$(DBUS_NAME).service
	install -Dm0644 data/cosmic.portal $(DESTDIR)$(datadir)/xdg-desktop-portal/portals/cosmic.portal
	install -Dm0644 data/cosmic-portals.conf $(DESTDIR)$(datadir)/xdg-desktop-portal/cosmic-portals.conf
	find 'data'/'icons' -type f -exec echo {} \; \
		| rev \
		| cut -d'/' -f-3 \
		| rev \
		| xargs -d '\n' -I {} install -Dm0644 'data'/'icons'/{} $(DESTDIR)$(iconsdir)/{}

## Cargo Vendoring

vendor:
	rm -rf .cargo
	mkdir -p .cargo
	cargo vendor | head -n -1 > .cargo/config.toml
	echo 'directory = "vendor"' >> .cargo/config.toml
	echo >> .cargo/config.toml
	echo '[env]' >> .cargo/config.toml
	if [ -n "$${SOURCE_DATE_EPOCH}" ]; then \
		source_date="$$(date -d "@$${SOURCE_DATE_EPOCH}" "+%Y-%m-%d")"; \
		echo "VERGEN_GIT_COMMIT_DATE = \"$${source_date}\"" >> .cargo/config.toml; \
	fi
	if [ -n "$${SOURCE_GIT_HASH}" ]; then \
		echo "VERGEN_GIT_SHA = \"$${SOURCE_GIT_HASH}\"" >> .cargo/config.toml; \
	fi
	tar pcf vendor.tar .cargo vendor
	rm -rf .cargo vendor

vendor-check:
ifeq ($(VENDOR),1)
	rm vendor -rf && tar xf vendor.tar
endif

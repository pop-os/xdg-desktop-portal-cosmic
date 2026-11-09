mod cargo 'cargo.just'

name := 'xdg-desktop-portal-cosmic'
appid := 'org.freedesktop.impl.portal.desktop.cosmic'
rootdir := ''
prefix := '/usr'
base-dir := absolute_path(clean(rootdir / prefix))

# Path baked into the installed service files (without rootdir)

libexecdir := clean(prefix / 'libexec')

# Installation target paths

data-dir := base-dir / 'share'
lib-dir := base-dir / 'lib'
icons-dir := data-dir / 'icons' / 'hicolor'
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
bin-src := cargo-target-dir / 'release' / name
bin-dst := absolute_path(clean(rootdir / libexecdir)) / name

default *args: (cargo::build-release args)

# Compiles with debug profile
build-debug *args: (cargo::build-debug args)

# Compiles with release profile
build-release *args: (cargo::build-release args)

# Compiles release profile with vendored dependencies
build-vendored *args: cargo::vendor-extract (cargo::build-vendored args)

# Removes the target directory
clean: cargo::clean

# Removes the target directory and vendored dependencies
clean-dist: cargo::clean-dist

# Runs a clippy check
check *args: (cargo::check args)

# Runs a clippy check with JSON message format
check-json: (check '--message-format=json')

# Vendor dependencies locally
vendor: cargo::vendor

# Extracts vendored dependencies
vendor-extract: cargo::vendor-extract

# Installs files
install:
    install -Dm0755 {{ bin-src }} {{ bin-dst }}
    sed 's|@libexecdir@|{{ libexecdir }}|' data/dbus-1/{{ appid }}.service.in \
        | install -Dm0644 /dev/stdin {{ data-dir }}/dbus-1/services/{{ appid }}.service
    sed 's|@libexecdir@|{{ libexecdir }}|' data/{{ appid }}.service.in \
        | install -Dm0644 /dev/stdin {{ lib-dir }}/systemd/user/{{ appid }}.service
    install -Dm0644 data/cosmic.portal {{ data-dir }}/xdg-desktop-portal/portals/cosmic.portal
    install -Dm0644 data/cosmic-portals.conf {{ data-dir }}/xdg-desktop-portal/cosmic-portals.conf
    find 'data'/'icons' -type f -exec echo {} \; \
        | rev \
        | cut -d'/' -f-3 \
        | rev \
        | xargs -d '\n' -I {} install -Dm0644 'data'/'icons'/{} {{ icons-dir }}/{}

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

# Set debug=1 to build with the debug profile instead of release
debug := '0'
target := if debug == '0' { 'release' } else { 'debug' }
profile-args := if debug == '0' { '--release' } else { '' }

# Set vendor=1 to build offline from the vendored tarball
vendor := '0'
vendor-args := if vendor == '0' { '' } else { '--frozen' }

bin-src := cargo-target-dir / target / name
bin-dst := absolute_path(clean(rootdir / libexecdir)) / name

[private]
default: build

# Builds the application
build: vendor-check
    cargo build {{ profile-args }} {{ vendor-args }} --bin {{ name }}

# Removes the target directory
clean:
    rm -rf {{ cargo-target-dir }}

# Removes the target directory and vendored dependencies
distclean: clean
    rm -rf .cargo vendor vendor.tar

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

# Vendors Cargo dependencies into a tarball
vendor:
    rm -rf .cargo
    mkdir -p .cargo
    cargo vendor | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    echo >> .cargo/config.toml
    echo '[env]' >> .cargo/config.toml
    if [ -n "${SOURCE_DATE_EPOCH}" ]; then \
        source_date="$(date -d "@${SOURCE_DATE_EPOCH}" "+%Y-%m-%d")"; \
        echo "VERGEN_GIT_COMMIT_DATE = \"${source_date}\"" >> .cargo/config.toml; \
    fi
    if [ -n "${SOURCE_GIT_HASH}" ]; then \
        echo "VERGEN_GIT_SHA = \"${SOURCE_GIT_HASH}\"" >> .cargo/config.toml; \
    fi
    tar pcf vendor.tar .cargo vendor
    rm -rf .cargo vendor

# Extracts the vendored dependencies when vendor=1
[private]
vendor-check:
    if [ '{{ vendor }}' != '0' ]; then \
        rm -rf vendor && tar xf vendor.tar; \
    fi

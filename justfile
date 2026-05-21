name := 'xdg-desktop-portal-cosmic'
export APPID := 'org.freedesktop.impl.portal.desktop.cosmic'

rootdir := ''
prefix := '/usr'

# Paths for the final installed system (used in service files)
libexecdir := clean(prefix / 'libexec')

# Paths for staging installation (includes rootdir)
base-dir := absolute_path(clean(rootdir / prefix))

export INSTALL_DIR := base-dir / 'share'

bin-dir := base-dir / 'libexec'
data-dir := base-dir / 'share'
lib-dir := base-dir / 'lib'
icons-dir := data-dir / 'icons' / 'hicolor'

cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
bin-src := cargo-target-dir / 'release' / name
bin-dst := bin-dir / name

# Default recipe which runs `just build-release`
[private]
default: build-release

# Runs `cargo clean`
clean:
    cargo clean

# `cargo clean` and removes vendored dependencies
clean-dist: clean
    rm -rf .cargo vendor vendor.tar

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Compiles release profile with vendored dependencies
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Runs a clippy check
check *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

# Runs a clippy check with JSON message format
check-json: (check '--message-format=json')

# Run with debug logs
run *args:
    env RUST_LOG=debug RUST_BACKTRACE=full cargo run --release {{args}}

# Installs files
install:
    install -Dm0755 {{bin-src}} {{bin-dst}}
    sed 's|@libexecdir@|{{libexecdir}}|' data/dbus-1/{{APPID}}.service.in \
        | install -Dm0644 /dev/stdin {{data-dir}}/dbus-1/services/{{APPID}}.service
    sed 's|@libexecdir@|{{libexecdir}}|' data/{{APPID}}.service.in \
        | install -Dm0644 /dev/stdin {{lib-dir}}/systemd/user/{{APPID}}.service
    install -Dm0644 data/cosmic.portal {{data-dir}}/xdg-desktop-portal/portals/cosmic.portal
    install -Dm0644 data/cosmic-portals.conf {{data-dir}}/xdg-desktop-portal/cosmic-portals.conf
    find 'data'/'icons' -type f -exec echo {} \; \
        | rev \
        | cut -d'/' -f-3 \
        | rev \
        | xargs -d '\n' -I {} install -Dm0644 'data'/'icons'/{} {{icons-dir}}/{}
    -pkill -f 'xdg-desktop-portal-cosmic'

# Uninstalls installed files
uninstall:
    rm -f {{bin-dst}}
    rm -f {{data-dir}}/dbus-1/services/{{APPID}}.service
    rm -f {{lib-dir}}/systemd/user/{{APPID}}.service
    rm -f {{data-dir}}/xdg-desktop-portal/portals/cosmic.portal
    rm -f {{data-dir}}/xdg-desktop-portal/cosmic-portals.conf
    find 'data'/'icons' -type f -exec echo {} \; \
        | rev \
        | cut -d'/' -f-3 \
        | rev \
        | xargs -d '\n' -I {} rm -f {{icons-dir}}/{}

# Vendor dependencies locally
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

# Extracts vendored dependencies
[private]
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar

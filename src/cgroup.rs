// SPDX-License-Identifier: GPL-3.0-only

use std::{collections::HashSet, io};

use tokio::fs;

static COSMIC_SCOPE: &str = "app-cosmic-";
static FLATPAK_SCOPE: &str = "app-flatpak-";

/// Returns appid if COSMIC or Flatpak launched the process in this cgroup scope.
///
/// COSMIC and Flatpak place launched apps in a cgroup scope named
/// `app-cosmic-{appid}-{PID}.scope` / `app-flatpak-{appid}-{PID}.scope`.
fn cosmic_flatpak_name(scope: &str) -> Option<&str> {
    scope
        .strip_prefix(COSMIC_SCOPE)
        .or_else(|| scope.strip_prefix(FLATPAK_SCOPE))?
        .rsplit_once('-')
        .and_then(|(appid, pid_scope)| {
            // Check if scope ends in `-{PID}.scope`
            _ = pid_scope.strip_suffix(".scope")?.parse::<u32>().ok()?;
            Some(appid)
        })
}

/// Extract the app id from the contents of a `/proc/{PID}/cgroup` file.
fn app_id_from_cgroup(contents: &str) -> Option<&str> {
    contents.lines().find_map(|line| {
        // Each line is `hierarchy-ID:controller-list:cgroup-path`. An app's
        // processes can live in a child cgroup of its scope, so scan every path
        // component for the `app-cosmic-{appid}-{PID}.scope` scope rather than
        // assuming it's the leaf.
        let path = line.rsplit_once(':').map(|(_, path)| path)?;
        path.split('/').find_map(cosmic_flatpak_name)
    })
}

/// Enumerate the appids of COSMIC/Flatpak apps that are running, by reading each
/// process's cgroup from `/proc/{PID}/cgroup`.
///
/// This reads the process cgroups directly instead of querying systemd over
/// D-Bus, so it also works on Linux distributions that aren't using systemd.
pub async fn running_app_ids() -> io::Result<HashSet<String>> {
    let mut proc = fs::read_dir("/proc").await?;
    let mut app_ids = HashSet::new();

    while let Some(entry) = proc.next_entry().await? {
        // Process directories are named after their PID.
        if entry
            .file_name()
            .to_str()
            .is_none_or(|name| name.parse::<u32>().is_err())
        {
            continue;
        }

        let cgroup = entry.path().join("cgroup");
        match fs::read_to_string(&cgroup).await {
            Ok(contents) => {
                if let Some(app_id) = app_id_from_cgroup(&contents) {
                    app_ids.insert(app_id.to_owned());
                }
            }
            // Processes come and go while we iterate; ignore ones that vanished.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => log::trace!("Skipping {}: {e}", cgroup.display()),
        }
    }

    Ok(app_ids)
}

#[cfg(test)]
mod tests {
    use super::{app_id_from_cgroup, cosmic_flatpak_name};

    const APPID: &str = "com.system76.CosmicFiles";

    #[test]
    fn parse_appid_without_scope_fails() {
        let name = cosmic_flatpak_name(APPID);
        assert!(
            name.is_none(),
            "Only apps launched by COSMIC or Flatpak should be parsed; got: {name:?}"
        );
    }

    #[test]
    fn parse_appid_with_scope_pid() {
        let scope = format!("app-cosmic-{APPID}-1234.scope");
        let name = cosmic_flatpak_name(&scope).expect("Should parse app launched by COSMIC");
        assert_eq!(APPID, name);
    }

    #[test]
    fn parse_appid_with_scope_no_pid_fails() {
        let scope = format!("app-cosmic-{APPID}.scope");
        let name = cosmic_flatpak_name(&scope);
        assert!(
            name.is_none(),
            "Apps launched by COSMIC/Flatpak should have a PID in its scope name"
        );
    }

    #[test]
    fn parse_appid_from_cgroup_v2() {
        let contents = format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cosmic-{APPID}-1234.scope\n"
        );
        assert_eq!(Some(APPID), app_id_from_cgroup(&contents));
    }

    #[test]
    fn parse_appid_from_cgroup_flatpak() {
        let contents = format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-{APPID}-99.scope\n"
        );
        assert_eq!(Some(APPID), app_id_from_cgroup(&contents));
    }

    #[test]
    fn parse_appid_from_nested_cgroup() {
        // A process sitting in a child cgroup beneath its app scope.
        let contents = format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cosmic-{APPID}-1234.scope/tab-7\n"
        );
        assert_eq!(Some(APPID), app_id_from_cgroup(&contents));
    }

    #[test]
    fn parse_appid_from_unrelated_cgroup_fails() {
        let contents = "0::/user.slice/user-1000.slice/session.slice/foo.service\n";
        assert!(app_id_from_cgroup(contents).is_none());
    }
}

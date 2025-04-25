use std::{
    collections::hash_map::{self, HashMap},
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

// TODO Purge gbm devices that are no longer needed/valid?
#[derive(Default)]
pub struct GbmDevices {
    devices: HashMap<u64, (PathBuf, gbm::Device<fs::File>)>,
}

impl GbmDevices {
    pub fn gbm_device(&mut self, dev: u64) -> io::Result<Option<(&Path, &gbm::Device<fs::File>)>> {
        Ok(match self.devices.entry(dev) {
            hash_map::Entry::Occupied(entry) => {
                let (path, gbm) = entry.into_mut();
                Some((path, gbm))
            }
            hash_map::Entry::Vacant(entry) => {
                if let Some(value) = find_gbm_device(dev)? {
                    let (path, gbm) = entry.insert(value);
                    Some((path, gbm))
                } else {
                    None
                }
            }
        })
    }
}

fn find_gbm_device(dev: u64) -> io::Result<Option<(PathBuf, gbm::Device<fs::File>)>> {
    for i in std::fs::read_dir("/dev/dri")? {
        let i = i?;
        if i.metadata()?.rdev() == dev {
            let file = fs::File::options().read(true).write(true).open(i.path())?;
            log::info!("Opened gbm main device '{}'", i.path().display());
            return Ok(Some((i.path(), gbm::Device::new(file)?)));
        }
    }
    Ok(None)
}

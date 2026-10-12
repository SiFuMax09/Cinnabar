//! Migration from version 3 to version 4, which updated all block state and item ids to MC 26.2

use super::{PlotData, PlotLoadError, PLOT_MAGIC};
use byteorder::{LittleEndian, ReadBytesExt};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use tracing::debug;

pub fn convert_v3_to_v4(path: impl AsRef<Path>) -> Result<Option<PlotData>, PlotLoadError> {
    let mut file = File::open(&path)?;

    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    if &magic != PLOT_MAGIC {
        return Ok(None);
    }

    let version = file.read_u32::<LittleEndian>()?;
    if version != 3 {
        return Ok(None);
    }

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let old_data: PlotData = bincode::deserialize(&buf)?;

    debug!("Successfully converted plot data from version 3 to version 4");
    Ok(Some(crate::upgrade::upgrade_plot(old_data)))
}

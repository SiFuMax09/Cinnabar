//! Migration from version 4 to version 5, which added the time of day and weather of the plot

use super::{ChunkData, EnvironmentData, PlotData, PlotLoadError, Tps, WorldSendRate, PLOT_MAGIC};
use byteorder::{LittleEndian, ReadBytesExt};
use mchprs_world::TickEntry;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use tracing::debug;

/// The plot data layout of versions 3 and 4, which only differed in their block ids.
#[derive(Deserialize)]
pub(super) struct PlotDataV4 {
    tps: Tps,
    world_send_rate: WorldSendRate,
    chunk_data: Vec<ChunkData>,
    pending_ticks: Vec<TickEntry>,
}

impl From<PlotDataV4> for PlotData {
    fn from(old: PlotDataV4) -> Self {
        PlotData {
            tps: old.tps,
            world_send_rate: old.world_send_rate,
            chunk_data: old.chunk_data,
            pending_ticks: old.pending_ticks,
            environment: EnvironmentData::default(),
        }
    }
}

pub fn convert_v4_to_v5(path: impl AsRef<Path>) -> Result<Option<PlotData>, PlotLoadError> {
    let mut file = File::open(&path)?;

    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    if &magic != PLOT_MAGIC {
        return Ok(None);
    }

    let version = file.read_u32::<LittleEndian>()?;
    if version != 4 {
        return Ok(None);
    }

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let old_data: PlotDataV4 = bincode::deserialize(&buf)?;

    debug!("Successfully converted plot data from version 4 to version 5");
    Ok(Some(old_data.into()))
}

#[cfg(test)]
mod tests {
    use super::super::WeatherData;
    use super::*;

    #[test]
    fn v4_plot_loads_with_default_environment() {
        let old = bincode::serialize(&(
            Tps::Limited(10.0),
            WorldSendRate(60.0),
            Vec::<ChunkData>::new(),
            Vec::<TickEntry>::new(),
        ))
        .unwrap();
        let path = std::env::temp_dir().join(format!("mchprs_v4_test_{}", std::process::id()));
        let mut file = PLOT_MAGIC.to_vec();
        file.extend_from_slice(&4u32.to_le_bytes());
        file.extend_from_slice(&old);
        std::fs::write(&path, file).unwrap();

        let data = convert_v4_to_v5(&path).unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(data.environment, EnvironmentData::default());
        assert!(data.environment.clock_frozen);
        assert_eq!(data.environment.clock_ticks, 6000);
        assert_eq!(data.tps, Tps::Limited(10.0));
    }

    #[test]
    fn environment_roundtrips() {
        let env = EnvironmentData {
            clock_ticks: 123_456,
            clock_frozen: false,
            weather: WeatherData {
                raining: true,
                thundering: true,
                ticks_left: 99,
            },
        };
        let bytes = bincode::serialize(&env).unwrap();
        assert_eq!(
            bincode::deserialize::<EnvironmentData>(&bytes).unwrap(),
            env
        );
    }
}

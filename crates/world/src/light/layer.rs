/// The light levels of the 16x16x16 blocks of a section, with 4 bits per block in the layout of
/// the protocol. Sections with the same level everywhere take no memory.
#[derive(Clone)]
pub enum Nibbles {
    Uniform(u8),
    Data(Box<[u8; 2048]>),
}

impl Nibbles {
    fn index(x: u32, y: u32, z: u32) -> usize {
        (y << 8 | z << 4 | x) as usize
    }

    pub fn get(&self, x: u32, y: u32, z: u32) -> u8 {
        match self {
            Nibbles::Uniform(level) => *level,
            Nibbles::Data(data) => {
                let idx = Self::index(x, y, z);
                data[idx >> 1] >> ((idx & 1) * 4) & 15
            }
        }
    }

    /// Returns true if the level changed
    pub fn set(&mut self, x: u32, y: u32, z: u32, level: u8) -> bool {
        let idx = Self::index(x, y, z);
        let data = match self {
            Nibbles::Uniform(current) if *current == level => return false,
            Nibbles::Uniform(current) => {
                *self = Nibbles::Data(Box::new([*current * 0x11; 2048]));
                let Nibbles::Data(data) = self else {
                    unreachable!()
                };
                data
            }
            Nibbles::Data(data) => data,
        };
        let shift = (idx & 1) * 4;
        let byte = &mut data[idx >> 1];
        if *byte >> shift & 15 == level {
            return false;
        }
        *byte = *byte & !(15 << shift) | level << shift;
        true
    }

    pub fn is_zero(&self) -> bool {
        match self {
            Nibbles::Uniform(level) => *level == 0,
            Nibbles::Data(data) => data.iter().all(|byte| *byte == 0),
        }
    }

    pub fn to_bytes(&self) -> Box<[u8; 2048]> {
        match self {
            Nibbles::Uniform(level) => Box::new([*level * 0x11; 2048]),
            Nibbles::Data(data) => data.clone(),
        }
    }
}

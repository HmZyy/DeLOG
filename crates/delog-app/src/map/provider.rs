use std::ops::RangeInclusive;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TileId {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn quadkey(self) -> String {
        let mut key = String::with_capacity(self.zoom as usize);
        for level in (1..=self.zoom).rev() {
            let mask = 1 << (level - 1);
            let digit = ((self.x & mask != 0) as u8) + 2 * ((self.y & mask != 0) as u8);
            key.push(char::from(b'0' + digit));
        }
        key
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MapProviderId {
    #[default]
    None,
    BingSatellite,
}

pub trait MapProvider: Send + Sync {
    fn label(&self) -> &'static str;
    fn zoom_range(&self) -> RangeInclusive<u8>;
    fn url(&self, tile: TileId) -> String;
}

struct BingSatellite;

impl MapProvider for BingSatellite {
    fn label(&self) -> &'static str {
        "Bing Satellite"
    }
    fn zoom_range(&self) -> RangeInclusive<u8> {
        1..=20
    }
    fn url(&self, tile: TileId) -> String {
        let server = (u64::from(tile.x) + 2 * u64::from(tile.y)) % 4;
        format!(
            "http://ecn.t{server}.tiles.virtualearth.net/tiles/a{}.jpeg?g=2981&mkt=en-US",
            tile.quadkey()
        )
    }
}

static BING_SATELLITE: BingSatellite = BingSatellite;

pub fn provider(id: MapProviderId) -> Option<&'static dyn MapProvider> {
    match id {
        MapProviderId::None => None,
        MapProviderId::BingSatellite => Some(&BING_SATELLITE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_quadkey_uses_bing_digit_order() {
        assert_eq!(
            TileId {
                zoom: 3,
                x: 3,
                y: 5
            }
            .quadkey(),
            "213"
        );
    }

    #[test]
    fn bing_url_uses_expected_host_type_and_quadkey() {
        let tile = TileId {
            zoom: 3,
            x: 3,
            y: 5,
        };
        assert_eq!(
            provider(MapProviderId::BingSatellite).unwrap().url(tile),
            "http://ecn.t1.tiles.virtualearth.net/tiles/a213.jpeg?g=2981&mkt=en-US"
        );
    }
}

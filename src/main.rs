use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File},
    io::{BufRead, Cursor, Read, Seek},
    path::{Path, PathBuf},
    str::FromStr,
};

use image::{GenericImageView, ImageFormat};
use serde::{
    de::{Unexpected, Visitor},
    Deserialize,
};
use structopt::StructOpt;
use zip::{read::ZipFile, ZipArchive};

#[derive(StructOpt)]
struct Opt {
    /// Path to folder with original mod files
    input_dir: PathBuf,
    /// Path to toml file, using headers as atlas names, keys as positions,
    /// and values as result names
    toml: PathBuf,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct AtlasPos(u8);

impl AtlasPos {
    fn from_pos(x: u8, y: u8) -> Self {
        Self((y << 4) | x)
    }
}

impl fmt::Debug for AtlasPos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let x = self.0 & 0xf;
        let y = self.0 >> 4;
        write!(f, "{:x}{:x}", y, x)
    }
}

enum ParseError {
    NotHexDigits,
    WrongSize(usize),
}

impl FromStr for AtlasPos {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.as_bytes() {
            [a, b] if a.is_ascii_hexdigit() && b.is_ascii_hexdigit() => {
                u8::from_str_radix(s, 16)
                    .map(AtlasPos)
                    .map_err(|_| ParseError::NotHexDigits)
            }
            [_, _] => Err(ParseError::NotHexDigits),
            bytes => Err(ParseError::WrongSize(bytes.len())),
        }
    }
}

struct AtlasPosVisitor;
impl<'v> Visitor<'v> for AtlasPosVisitor {
    type Value = AtlasPos;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("expecting two hex digits")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        AtlasPos::from_str(v).map_err(|e| match e {
            ParseError::NotHexDigits => {
                E::invalid_value(Unexpected::Str(v), &"hex digit")
            }
            ParseError::WrongSize(len) => E::invalid_length(len, &"2"),
        })
    }
}

impl<'de> Deserialize<'de> for AtlasPos {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(AtlasPosVisitor)
    }
}

type AtlasMap = BTreeMap<String, Atlas>;
type Atlas = BTreeMap<AtlasPos, String>;
type Folders = BTreeMap<String, Vec<String>>;

#[derive(Debug, Deserialize)]
struct Toml {
    modid: String,
    banner: String,
    models: Vec<String>,
    gui: Vec<String>,
    blocks_copy: Vec<String>,
    imgs: Vec<String>,
    bin: String,
    folders: Folders,
    blocks: AtlasMap,
    items: AtlasMap,
}

fn process_atlas<R: BufRead + Seek>(
    atlas: &Atlas,
    input: R,
    output_dir: &Path,
) -> anyhow::Result<()> {
    let image = image::load(input, ImageFormat::Png)?.to_rgba8();
    for y in 0..16 {
        for x in 0..16 {
            let slice = AtlasPos::from_pos(x as u8, y as u8);
            if let Some(output) = atlas.get(&slice) {
                let output = output_dir.join(output).with_extension("png");
                image
                    .view(x * 16, y * 16, 16, 16)
                    .to_image()
                    .save_with_format(output, ImageFormat::Png)?;
            }
        }
    }
    Ok(())
}
fn process_atlas_map(
    atlas: &AtlasMap,
    zips: &mut Zips,
    output_dir: &Path,
) -> anyhow::Result<()> {
    for (atlas, map) in atlas {
        let path = Path::new(atlas).with_extension("png");
        let name = path.to_str().unwrap();
        let mut image = zips.find(name).unwrap();
        let mut data = Vec::with_capacity(image.size() as usize);
        image.read_to_end(&mut data)?;
        process_atlas(map, Cursor::new(data), output_dir)?;
    }
    Ok(())
}

struct Zips<'a>(Vec<(ZipArchive<File>, &'a [String])>);

// Yes, this is dumb, I don't care
unsafe fn cheat_lifetime<'a, 'b>(t: ZipFile<'a>) -> ZipFile<'b> {
    std::mem::transmute(t)
}

impl<'a> Zips<'a> {
    fn new(folders: &'a Folders, input_dir: &Path) -> anyhow::Result<Self> {
        let zips = folders
            .iter()
            .map(|(file, paths)| -> anyhow::Result<_> {
                Ok((
                    zip::ZipArchive::new(File::open(input_dir.join(file))?)?,
                    &paths[..],
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self(zips))
    }

    fn find(&mut self, file: &str) -> Option<ZipFile> {
        for (zip, paths) in self.0.iter_mut() {
            for path in paths.iter() {
                if let Ok(file) = zip.by_name(&format!("{}/{}", path, file)) {
                    return Some(unsafe { cheat_lifetime(file) });
                }
            }
        }
        None
    }
}

fn main() -> anyhow::Result<()> {
    let opt = Opt::from_args();
    let toml = fs::read_to_string(&opt.toml)?;
    let toml: Toml = toml::from_str(&toml)?;
    let mut zips = Zips::new(&toml.folders, &opt.input_dir)?;
    let res = opt
        .toml
        .parent()
        .unwrap()
        .join("src")
        .join("main")
        .join("resources");
    let namespace = res.join("assets").join(toml.modid);
    let textures = namespace.join("textures");
    let models_dir = namespace.join("models").join("block");
    let guis_dir = textures.join("gui");
    let blocks_dir = textures.join("block");
    let items_dir = textures.join("item");

    let mut banner = zips.find(&toml.banner).unwrap();
    let mut banner_file = File::create(res.join(toml.banner))?;
    std::io::copy(&mut banner, &mut banner_file)?;
    drop(banner);

    for model in toml.models {
        let mut model_file = zips.find(&model).unwrap();
        let mut file = File::create(models_dir.join(model))?;
        std::io::copy(&mut model_file, &mut file)?;
    }

    for gui in toml.gui {
        let mut image = zips.find(&gui).unwrap();
        let mut file = File::create(guis_dir.join(gui))?;
        std::io::copy(&mut image, &mut file)?;
    }

    for block in toml.blocks_copy {
        let mut image = zips.find(&block).unwrap();
        let mut file = File::create(blocks_dir.join(block))?;
        std::io::copy(&mut image, &mut file)?;
    }

    process_atlas_map(&toml.items, &mut zips, &items_dir)?;
    process_atlas_map(&toml.blocks, &mut zips, &blocks_dir)?;

    println!("done");
    Ok(())
}

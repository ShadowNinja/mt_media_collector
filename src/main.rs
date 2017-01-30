#[macro_use] extern crate clap;
extern crate ini;
extern crate sha1;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write, BufWriter};
use std::iter::FromIterator;
use std::path::{Path, PathBuf};

use ini::Ini;
use ini::ini::Error as IniError;


type Sha1DigestBytes = [u8; 20];
type ModList = Vec<String>;
type MediaSet = Vec<Asset>;


struct Asset {
	path: PathBuf,
	hash: Sha1DigestBytes,
}

impl Asset {
	pub fn new(pb: PathBuf, h: Sha1DigestBytes) -> Self {
		Asset {
			path: pb,
			hash: h,
		}
	}
}

impl PartialEq for Asset {
	fn eq(&self, other: &Self) -> bool {
		self.hash == other.hash
	}
}


enum Error {
	Io(io::Error),
	Ini(IniError),
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			Error::Io(ref e)  => write!(f, "IO error: {}", e),
			Error::Ini(ref e) => write!(f, "Settings file error: {}", e),
		}
	}
}

impl From<io::Error> for Error {
	fn from(e: io::Error) -> Self { Error::Io(e) }
}

impl From<IniError> for Error {
	fn from(e: IniError) -> Self { Error::Ini(e) }
}


enum AssetCopyMode {
	Symlink,
	Hardlink,
	Copy,
	None,
}


fn to_hex(input: &[u8]) -> String {
	String::from_iter(input.iter().map(|b| format!("{:02x}", b)))
}


fn make_absolute(path: &Path) -> PathBuf {
	if path.is_absolute() {
		path.to_path_buf()
	} else {
		std::env::current_dir()
			.and_then(|cd| Ok(cd.join(path)))
			.or_else(|_err| -> io::Result<_> {
				Ok(path.to_path_buf())
			})
			.unwrap()
	}
}


fn hash_file(path: &Path) -> io::Result<Sha1DigestBytes> {
	let mut buf = [0u8; 8192];
	let mut hash = sha1::Sha1::new();
	let mut file = File::open(&path)?;
	loop {
		match file.read(&mut buf) {
			Ok(0) => break,
			Ok(len) => hash.update(&buf[..len]),
			Err(e) => return Err(e),
		}
	}
	Ok(hash.digest().bytes())
}


fn search_media_dir(ms: &mut MediaSet, path: &Path) -> io::Result<()> {
	for entry in path.read_dir()? {
		let pb = entry?.path();
		if pb.is_file() {
			let h = hash_file(pb.as_path())?;
			ms.push(Asset::new(pb, h));
		}
	}
	Ok(())
}


fn search_mod_dir(ms: &mut MediaSet, path: &Path) -> io::Result<()> {
	static MEDIA_DIRS: &'static [&'static str] = &["textures", "models", "sounds"];
	for media_dir in MEDIA_DIRS {
		let media_pb = path.join(media_dir);
		if media_pb.is_dir() {
			search_media_dir(ms, media_pb.as_path())?;
		}
	}
	Ok(())
}


fn search_modpack_dir(ms: &mut MediaSet, path: &Path, mods: Option<&ModList>) -> io::Result<()> {
	for entry in path.read_dir()? {
		let entry_path = entry?.path();
		if !entry_path.is_dir() {
			continue;
		} else if entry_path.join("modpack.txt").exists() {
			search_modpack_dir(ms, entry_path.as_path(), mods)?;
		} else if entry_path.join("init.lua").exists() {
			if let Some(mod_list) = mods {
				let mod_name = &entry_path.file_name()
					.expect("Mod directory has no name!")
					.to_str()
					.expect("Mod directory name is not valid Unicode")
					.to_string();
				if !mod_list.contains(mod_name) {
					continue;
				}
			}
			search_mod_dir(ms, entry_path.as_path())?;
		}
		// Otherwise it's probably a VCS directory or something similar
	}
	Ok(())
}


fn write_index(ms: &MediaSet, path: &Path) -> io::Result<()> {
	let file = File::create(&path)?;
	let mut writer = BufWriter::new(file);
	writer.write_all(b"MTHS\x00\x01")?;
	for asset in ms {
		writer.write_all(&asset.hash)?;
	}
	Ok(())
}


fn copy_assets(ms: &MediaSet, path: &Path, mode: AssetCopyMode) -> io::Result<()> {
	fn copy_no_result<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
		fs::copy(src, dst).map(|_| ())
	}

	#[cfg(unix)]
	fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
		std::os::unix::fs::symlink(src, dst)
	}

	#[cfg(windows)]
	fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
		std::os::windows::fs::symlink_file(src, dst)
	}

	#[cfg(not(any(unix, windows)))]
	fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> io::Result<()> {
		Err(io::Error::new(io::ErrorKind::Other,
				"Symlinking not supported on this platform!"))
	}

	let copy_func = match mode {
		AssetCopyMode::Symlink => symlink_file,
		AssetCopyMode::Hardlink => fs::hard_link,
		AssetCopyMode::Copy => copy_no_result,
		AssetCopyMode::None => return Ok(()),
	};

	for asset in ms {
		copy_func(&asset.path, path.join(to_hex(&asset.hash)))?;
	}
	Ok(())
}


fn get_mod_list(path: &Path) -> Result<ModList, IniError> {
	let world_mt = Ini::load_from_file(path.join("world.mt"))?;
	let main_sec = world_mt.general_section();

	let mut list: ModList = vec![];
	for (key, value) in main_sec {
		if !key.starts_with("load_mod_") || value != "true" {
			continue;
		}
		let (_, mod_name) = key.split_at(9);
		list.push(mod_name.to_string());
	}
	Ok(list)
}


fn get_args<'a>() -> clap::ArgMatches<'a> {
	use clap::{App, Arg, ArgGroup};

	fn check_new_dir(s: &OsStr) -> Result<(), OsString> {
		let p = make_absolute(Path::new(&s));
		if p.is_dir() {
			return Ok(())
		}
		if let Some(parent) = p.parent() {
			if parent.is_dir() {
				return Ok(())
			}
		}
		return Err("Invalid path.".into())
	}

	fn check_existing_dir(s: &OsStr) -> Result<(), OsString> {
		if make_absolute(Path::new(&s)).is_dir() {
			Ok(())
		} else {
			Err("Invalid path.".into())
		}
	}

	let app = clap_app! { @app (app_from_crate!())
		(version_short: "v")

		(@arg mod_paths: [PATHS] ... validator_os(check_existing_dir) "Additional mod paths to search.")

		(@arg out:   -o --out   <PATH> validator_os(check_new_dir) "Path to the output directory.")
		(@arg world: -w --world <PATH> validator_os(check_existing_dir) "Path to the world directory.")
		(@arg game:  -g --game  <PATH> validator_os(check_existing_dir) "Path to the game directory.")

		// Group these together with display_order
		(@arg copy: -c --copy display_order(1000) "Copy assets to output folder.")
		// Symlink added below if applicable
		(@arg hardlink: -l --hardlink display_order(1000) "Hard link assets to output folder.")
	};


	// Add symlink option if supported
	#[cfg(not(any(unix, windows)))]
	let add_symlink_arg = |app| app;

	#[cfg(any(unix, windows))]
	fn add_symlink_arg<'a>(app: App<'a, 'a>) -> App<'a, 'a> {
		app.arg(Arg::with_name("symlink")
			.short("s")
			.long("symlink")
			.display_order(1000)
			.help("Symbolically link assets to output folder."))
	}

	add_symlink_arg(app)
		// Link group has to be added manually because the
		// symlink argument is added conditionally.
		.group(ArgGroup::with_name("link")
			.args(&["copy", "symlink", "hardlink"]))

		.get_matches()
}


fn run(args: clap::ArgMatches) -> Result<(), Error> {
	// These unwraps are safe since the values are required
	// and clap will exit if the value is missing.
	let out_opt = args.value_of_os("out").unwrap();
	let out_path = Path::new(&out_opt);
	let world_opt = args.value_of_os("world").unwrap();
	let world_path = Path::new(&world_opt);
	let game_opt = args.value_of_os("game").unwrap();
	let game_path = Path::new(&game_opt);

	let copy_type = if args.is_present("copy") {
			AssetCopyMode::Copy
		} else if args.is_present("symlink") {
			AssetCopyMode::Symlink
		} else if args.is_present("hardlink") {
			AssetCopyMode::Hardlink
		} else {
			AssetCopyMode::None
		};

	let mut ms = MediaSet::new();
	let mods = get_mod_list(world_path)?;

	// Search world mods.
	let worldmods_path = world_path.join("worldmods");
	if worldmods_path.exists() {
		search_modpack_dir(&mut ms, worldmods_path.as_path(), Some(&mods))?;
	}

	// Search game mods.
	// Note: Game mods can not currently be disabled.
	search_modpack_dir(&mut ms, game_path.join("mods").as_path(), None)?;

	if let Some(mod_paths) = args.values_of_os("mod_paths") {
		for mod_path in mod_paths {
			search_modpack_dir(&mut ms,
					Path::new(&mod_path),
					Some(&mods))?;
		}
	}

	// Deduplicate list.  Otherwise linking will fail and the index will
	// be unnecessarily large.
	ms.sort_by(|a, b| a.hash.cmp(&b.hash));
	ms.dedup();

	if !out_path.exists() {
		fs::create_dir(out_path)?;
	}

	write_index(&ms, out_path.join("index.mth").as_path())?;

	copy_assets(&ms, out_path, copy_type)?;

	Ok(())
}


fn main() {
	match run(get_args()) {
		Ok(()) => return,
		Err(e) => {
			println!("{}", e);
			std::process::exit(1)
		}
	}
}

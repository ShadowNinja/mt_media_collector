extern crate getopts;
extern crate ini;
extern crate sha1;

use std::env;
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


fn handle_args() -> Option<getopts::Matches> {
	let mut args = env::args();
	let cmd_name = args.next()
		.and_then(|opt| {
			Path::new(&opt)
				.file_name()
				.and_then(|os_fn| os_fn.to_str())
				.and_then(|s| Some(s.to_string()))
		})
		.unwrap_or(env!("CARGO_PKG_NAME").to_string());

	let mut opts = getopts::Options::new();
	opts.optflag("h", "help", "Print this help menu.");
	opts.reqopt("o", "out", "Path to the output directory.", "PATH");
	opts.reqopt("w", "world", "Path to the world directory.", "PATH");
	opts.reqopt("g", "game", "Path to the game directory.", "PATH");
	opts.optflagopt("c", "copy",
			"Copy assets to folder. Takes optional copy method: \
				one of 'symlink', 'hardlink', 'copy' (default)",
			"METHOD");

	let usage = || {
		opts.usage(&format!("{} [mod paths]\n\n\
			Collects assets for a Minetest HTTP asset server and \
			creates an index.mth file for those assets.",
			opts.short_usage(cmd_name.as_str())
		))
	};

	let matches = match opts.parse(args) {
		Ok(matches) => matches,
		Err(fail) => {
			print!("Error: {}\n\n{}", fail.to_string(), usage());
			return None;
		}
	};

	if matches.opt_present("h") {
		print!("{}", usage());
		return None;
	}

	return Some(matches);
}


fn int_main() -> i32 {
	macro_rules! handle_result {
		( $x:expr, $m:expr ) => {
			match $x {
				Ok(val) => val,
				Err(e) => {
					println!($m, e);
					return 1;
				}
			};
		}
	}

	let args = match handle_args() {
		Some(a) => a,
		None => return 1,
	};

	let out_opt = args.opt_str("out").unwrap();
	let out_path = Path::new(&out_opt);
	let world_opt = args.opt_str("world").unwrap();
	let world_path = Path::new(&world_opt);
	let game_opt = args.opt_str("game").unwrap();
	let game_path = Path::new(&game_opt);

	let abs_out_path = make_absolute(out_path);
	let out_parent = abs_out_path.parent();
	if out_parent.is_some() && !out_parent.unwrap().exists() {
		println!("Error: Out path parent should exist.");
		return 1;
	}

	let copy_type = match args.opt_default("copy", "copy") {
		None => AssetCopyMode::None,
		Some(t) => {
			if t == "symlink" {
				AssetCopyMode::Symlink
			} else if t == "hardlink" {
				AssetCopyMode::Hardlink
			} else if t == "copy" {
				AssetCopyMode::Copy
			} else {
				println!("Error: Invalid value for copy argument.");
				return 1;
			}
		}
	};

	let mut ms = MediaSet::new();
	let mods = handle_result!(get_mod_list(world_path), "Error getting mod list: {}");

	// Search world mods.
	let worldmods_path = world_path.join("worldmods");
	if worldmods_path.exists() {
		handle_result!(search_modpack_dir(&mut ms, worldmods_path.as_path(), Some(&mods)),
				"Error searching world mods: {}");
	}

	// Search game mods.
	// Note: Game mods can not currently be disabled.
	handle_result!(search_modpack_dir(&mut ms, game_path.join("mods").as_path(), None),
			"Error searching game mods: {}");

	for mod_path in args.free {
		handle_result!(search_modpack_dir(&mut ms, Path::new(&mod_path), Some(&mods)),
				"Error searching other mods: {}");
	}

	// Deduplicate list.  Otherwise linking will fail and the index will
	// be unnecessarily large.
	ms.sort_by(|a, b| a.hash.cmp(&b.hash));
	ms.dedup();

	if !out_path.exists() {
		handle_result!(fs::create_dir(out_path), "{}");
	}

	handle_result!(write_index(&ms, out_path.join("index.mth").as_path()),
			"Error writing asset index: {}");

	handle_result!(copy_assets(&ms, out_path, copy_type),
			"Error linking assets: {}");

	0
}


fn main() {
	// Rust (stable) only supports setting the exit code by calling this
	// function, but it doesn't do cleanup, so it can't be run until all
	// resources have already been destroyed.
	std::process::exit(int_main())
}

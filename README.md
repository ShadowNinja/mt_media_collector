Minetest media collector
====

This generates an `index.mth` file to be served over HTTP to Minetest clients
and optionally collects all of the necessary media files into a directory.

Installation
----

Simply install rust (and cargo).  You can build the binary with `cargo build`
or just use `cargo run` as below and the project will be built automatically.

Examples
----

List all options:
```
$ cargo run -- --help
```

Copys all media files to /srv/http/mt and save an index there.
```
$ cargo run -- --copy \
	--world ~/.minetest/worlds/world \
	--game ~/.minetest/games/minetest_game \
	--out /srv/http/mt
```

Hard link all media files in /srv/http/mt/media and add an index in /srv/http/mt/foo:
```
$ cargo run -- --hardlink \
	--world ~/.minetest/worlds/world \
	--game ~/.minetest/games/minetest_game \
	--media /srv/http/mt/media \
	--index /srv/http/mt/foo/index.mth
```

Symlink all media files in /srv/http/mt/media:
```
$ cargo run -- --symlink \
	--world ~/.minetest/worlds/world \
	--game ~/.minetest/games/minetest_game \
	--media /srv/http/mt/media
```

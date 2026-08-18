# Minimap tiles: `md5translate.trs` and the art it names

Every terrain tile has a 256x256 picture of itself baked at build time, and
**none of them is stored under its own name.** They sit in `Textures\Minimap\`
named by the MD5 of their contents; `Textures\Minimap\md5translate.trs` is the
only thing that says which hash belongs to `Azeroth\map32_48.blp`. Without it
the art is 18,536 indistinguishable files.

Reader: `crates/adt/src/minimap.rs`. Tools: `wow-cli minimap index | tiles |
orient | seams | stitch | export`.

## Shape

CRLF text, 19,090 lines, two kinds:

```
dir: Azeroth
Azeroth\map32_48.blp	b53fb722839e0c7a81bae678ea694f5c.blp
```

Every picture is 256x256 DXT1 with no mipmaps -- 33,940 bytes on disk, which is
a 1,172-byte BLP header and exactly one 32,768-byte level.

## What was measured

**The `dir:` headers are redundant, so the parser ignores them.** Every one of
the 18,644 entries carries its own full directory, and it agrees with the
header above it on **18,644 of 18,644**. A stateless parser and a state-tracking
one therefore build the same index, and only one of them can desynchronise.

**One picture can serve many tiles.** 18,644 entries name 14,420 distinct
files; 337 files are named more than once and the flat black tile under the
world's unreachable corners is named **1,127 times**. So the index is one-way:
a file name does not identify a tile, and a directory listing cannot replace
this file.

**Every reference resolves.** All 14,420 hashes exist in a 12340 install's
archive chain, checked by *path resolution* rather than by listing --
`wow-cli minimap index --verify`. A further 4,116 hash-named files exist that
nothing references, leftovers of earlier patches, so an index built by listing
the folder would be 22% noise with nothing to place any of it by.

**Of 445 directories, 63 hold terrain.** The rest hold per-WMO art
(`WMO\Azeroth\Buildings\Castle\castle01_000_00_00.blp`) for building
interiors, which this client does not draw. They are parsed and counted rather
than filtered out at parse time: a survey that cannot see them cannot report
that they are there.

## Which of `map<a>_<b>` is which, and how the tile *sets* said so

Both readings resolve a file for every tile, so no single lookup can separate
them. The **set** can: a continent's tiles are not symmetric under exchanging
the pair. `wow-cli minimap tiles`, over every map:

```
map                      terrain minimap   as written   transposed
Azeroth                      687     687          687          326
Kalimdor                     988    1018          988          479
Northrend                   1131    1131         1131          729
Expansion01                  800     800          800          234
...
66 maps, 5744 terrain tiles, 5321 tiles named
  as written: 5228 tiles land on real terrain, 43 maps exact
  transposed: 2578 tiles land on real terrain, 6 maps exact
  46 of 66 maps can tell the two readings apart at all
```

The six maps that "match transposed" are the square instance maps whose tile
sets are symmetric -- `DeadminesInstance`, `DrakTheronKeep`, `StratholmeCOT`.
They agree with both orders and vote for neither, which is why the last line
counts the maps that can separate them at all. Same population question as the
`MH2O` axis survey, where 86,222 full-chunk ocean sheets transpose to
themselves and had to be excluded before the vote meant anything.

A map having *more* art than terrain (Kalimdor 1018 named against 988 tiles) is
not a miscount: those are pictures for tiles a later patch removed from the
`WDT`. A map having less (`EmeraldDream` 91 of 256) is an unfinished map.

## Which way up the picture is, asked twice

A 256x256 tile has **eight plausible readings** -- either axis can run either
way and they can be exchanged -- and every one of them draws a picture. This is
the shape this project has paid for before, so it was settled twice, against
two inputs that share nothing.

### One: score the art against the water in the terrain

`MH2O` says which of a tile's 256 chunks are under water and water is drawn
blue, so each candidate is scored by whether "this chunk is covered" predicts
"this 16x16 block is blue". A chunk votes only if it is unambiguous (fully
covered or dry, nothing between) and only if the candidates disagree about it
-- a tile that is all ocean or all forest agrees with every reading.

`wow-cli minimap orient`, every map, 604,772 chunks classified and 102,334
decisive:

```
reading                             all   decisive   wet-dry blueness
as drawn                          93.9%      84.3%               82.0
across flipped                    89.3%      57.2%               70.9
down flipped                       89.8%     60.3%               72.5
both flipped                      87.3%      45.6%               66.4
transposed                        89.9%      60.5%               72.2
transposed, across flipped        89.1%      56.3%               70.6
transposed, down flipped          89.1%      56.3%               70.6
transposed, both flipped          89.7%      59.7%               71.9
```

The last column is threshold-free -- mean `blue - red` over covered chunks
minus the same over dry ones -- so the answer does not rest on where the blue
cutoff was put. 84.3% rather than 99% is expected and is not slack in the
reading: a chunk under six inches of river draws mostly riverbed, and a 16x16
block is a coarse thing to ask a colour question of. What matters is the
distance to the runner-up.

### Two: ask only whether the tiles join up

Under the right reading the last column of one tile's art is the ground
immediately beside the first column of its neighbour's; under a flipped one it
is two pieces of ground five hundred yards apart. This reads **no terrain at
all**, so it and the water test share nothing but the tile grid.

`wow-cli minimap seams`, every map, 13,027 seams:

```
reading                          across       down        sum
as drawn                           8.63      10.58      19.21
across flipped                    44.33      10.58      54.91
down flipped                       8.63      44.36      52.99
both flipped                      44.33      44.36      88.69
transposed                        42.00      41.72      83.72
transposed, across flipped        42.00      42.20      84.20
transposed, down flipped          41.91      41.72      83.63
transposed, both flipped          41.91      42.20      84.11
```

**Neither column settles it alone, and the numbers show that they cannot.** A
reading that flips only the down axis walks the same across-seam backwards and
scores *identically* on it -- 8.63 against 8.63 -- and one that flips only
across does the same to the down column. That degeneracy was predicted before
the run and appearing exactly where predicted is what says the experiment
measures what it claims. It is the pair together that separates all eight.

### The reading

`(0, 0)` of the art is the corner with the largest world `x` and `y`. Across
runs with falling `y` (west to east) and down with falling `x` (north to
south) -- the same convention `dbc::worldmap` fitted for a map page, arrived at
independently.

`wow-cli minimap stitch --x=-8950 --y=-132 --range 400` draws it: Northshire
Abbey, its road and its bridge, composed from four tiles with no visible seam,
and the centre marker on the grass by the abbey door where a human character
logs in.

## Not implemented

- WMO interior minimaps (the 382 non-terrain directories).
- Nothing reads the pictures' alpha channel; they are opaque.

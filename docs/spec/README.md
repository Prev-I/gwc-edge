# Published GWebCache specifications

This directory keeps the written GWebCache specifications that `gwc-edge` is
measured against, so that the comparison in `docs/protocol.md` still has its
sources if the original sites go away.

These are **references, not the authority.** `docs/protocol.md` is the
authority. It describes GWebCache v2 as Shareaza 2.7 actually parses it, and
its section
[Conformance to the published specifications](../protocol.md#conformance-to-the-published-specifications)
maps each clause below to what `gwc-edge` does, and says why where it differs.

## What is here

| File | Document | Version | Licence |
|---|---|---|---|
| `gwc1-specs.html` | Hauke Dämpfling, *Gnutella Web Caching System*: the original ("version 1") specification | 1.3.1, 6 July 2002 | GNU FDL 1.1 |
| `gwc2-newgwc.html` | Hauke Dämpfling, *Gnutella Web Caching System — Version 2 Specifications Client Developers' Guide* ("GWC2", which `gwc-edge` implements) | 1.9.4 (beta), 18 June 2003 | GNU FDL 1.1 |
| `doc_license.txt` | GNU Free Documentation License 1.1, the file both documents' footers link to, under its original name | March 2000 | verbatim copying permitted |

Provenance. All three files were retrieved on 2026-10-24 from
`https://www.gnucleus.org/gwebcache/` and are byte-identical to the
copies at `http://gnucleus.sourceforge.net/gwebcache/`. The documents give
their own addresses under `http://www.gnucleus.com/gwebcache/` (GWC2:
`newgwc.html`).

Their footer links to the licence (`doc_license.txt`) resolve here, because
the licence keeps its original file name.

| File | Original | Wayback Machine | SHA-256 |
|---|---|---|---|
| `gwc1-specs.html` | <https://www.gnucleus.org/gwebcache/specs.html> | <http://web.archive.org/web/20250122012455/https://gnucleus.sourceforge.net/gwebcache/specs.html> | `469880f7fb90e00f8fdf1bc301f7d108303a3903fab4a05af433b068a1c099f2` |
| `gwc2-newgwc.html` | <https://www.gnucleus.org/gwebcache/newgwc.html> | <http://web.archive.org/web/20260525165924/https://gnucleus.org/gwebcache/newgwc.html> | `2795d933b27205e9a63496a2522056dd73e5622c4661ad599b1a6245e65ca816` |
| `doc_license.txt` | <https://www.gnucleus.org/gwebcache/doc_license.txt> | | `18b188a70de5795fddfb424f26817858ffab83e22dbc1903a5eb5b7aba90b980` |

The files are kept exactly as served: ISO-8859-1, with their original HTML.
The stylesheet they reference was not copied, so open them in a browser for
reading. `.gitattributes` marks them `-text`, so Git never rewrites their
line endings and the hashes above stay checkable:

```bash
sha256sum docs/spec/*.html docs/spec/*.txt
```

## Licence

The MIT licence of this repository does **not** cover the three copied files.
This `README.md` is the project's own text, under MIT.

- **The two specifications** are Copyright (c) 2003 Hauke Dämpfling. Their
  footers name the "FDL" and link to the GNU Free Documentation License 1.1,
  which is the licence they are distributed under.
- **The licence text** is Copyright (C) 2000 Free Software Foundation, which
  permits verbatim copies of it.
- **The copies are verbatim** (FDL section 2), with their copyright and licence
  notices intact and the licence text alongside them in `doc_license.txt`.
- **Do not edit them.** Corrections, summaries and notes belong in
  `docs/protocol.md`. Changing these files would make them modified versions
  under the FDL, with obligations of their own.

## Referenced but not copied

**Shareaza Wiki, *GWC specs*.**
- Permalink: <https://shareaza.sourceforge.net/mediawiki/index.php?title=GWC_specs&oldid=11518>, last edited 10 January 2016.
- Wayback Machine: <http://web.archive.org/web/20241001004225/https://shareaza.sourceforge.net/mediawiki/GWC_specs>
- It describes "specification 2.1", the extension Shareaza uses: the `getleaves`, `getvendors` and `getuptime` flags, the extra `h|` fields, `x_leaves`, `x_max` and `uptime`, `getnetworks`, and the cache-to-cache `multi` ping. It also lists the replies Shareaza reacts to.

It is not copied, for two reasons:
- **Its licence.** It is published under Creative Commons
  Attribution-NonCommercial-ShareAlike 3.0. Its NonCommercial term would
  restrict redistribution of this repository in a way the MIT licence does
  not.
- **Its state.** The page itself says it was recovered from a backup and not
  verified. Some sections appear two or three times, and it numbers the
  extended `h|` fields in two contradictory ways.

`docs/protocol.md` takes those facts from Shareaza's source instead, which the
wiki only describes.

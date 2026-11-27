# Wallpaper assets

Only the small JPEG previews are bundled in Settings. Selecting a wallpaper
fetches its original from this public repository and caches the verified file.
Opening the gallery does not download originals; cached selections work offline.

## Publishing and adding images

- Commit and push `apps/settings/wallpapers/` to `main` before distributing the
  app that references new images. The downloader uses this directory on
  `raw.githubusercontent.com`; no token or user setup is required.
- Keep published files at the root with their original bytes so older app
  versions retain their verified download URLs. To replace a wallpaper in the
  current gallery without changing its ID, put the new JPEG in `replacements/`
  with the same filename. The generated catalog points the new app to that
  versioned path while older apps continue using the original.
- Append new wallpapers with new IDs; do not rename or reuse a published ID.
- Run `python scripts/prepare-wallpapers.py --import-directory <folder>` to append
  JPEGs, generate previews up to 400 × 400 without distortion and update the
  native integrity manifest and frontend catalog. Add the new display names in
  all eight Settings translations. `--check` verifies generated assets.

The 24 active wallpapers total **18,533,151 bytes** and are excluded from the
executable. The bundled previews total **337,241 bytes**, with metadata removed
and JPEG quality 82. The actual installer size also depends on code and
compression.

## Provenance and integrity

Images 01–12 were originally downloaded on 2026-11-23 from
[the requested gallery](https://openvid.dev/images/backgrounds/desktop/).
Current replacements for IDs 02, 03, 05–08, 10 and 12 were supplied by the
maintainer on 2026-11-26; they use `replacements/` paths so older app versions
can still download the original bytes at the root. Image 02 was converted from
PNG to JPEG. Images 13–20 were supplied by the maintainer on 2026-11-24, and
21–24 on 2026-11-26.

| File | Original source/name | Bytes | SHA-256 |
| --- | --- | ---: | --- |
| desktop-01.jpg | openvid.dev / desktop-01.jpg | 60977 | 1f1fbbf15e3b8ba121efaad287d0911abdf2db3bbf50f721caa881675facc7be |
| desktop-02.jpg | replacements/desktop-02.jpg | 144898 | 00526e782eeaea20132b7bb65da8c4f77964ed7c02aaa6fcb7e38c159331f3de |
| desktop-03.jpg | replacements/desktop-03.jpg | 2492611 | 511e766702a38926d6b57bb1a6baaf2bb2148c6cd6028d808f4195ebb9fe1648 |
| desktop-04.jpg | openvid.dev / desktop-04.jpg | 44823 | c64f72dd33fafe6ab61dc5d6a92cea88f672db2b3f88b284683c8e468012b8d2 |
| desktop-05.jpg | replacements/desktop-05.jpg | 430361 | 5bd13d7e471b3bc9fd564ae12c758654e96ad64c1df97c7e876c4c332dbb87c4 |
| desktop-06.jpg | replacements/desktop-06.jpg | 747688 | 13291f01dbeabcd274e9930ea93c32fef131712b94e565a4531f14126327b180 |
| desktop-07.jpg | replacements/desktop-07.jpg | 825828 | 84f9d7536c3f0b7541794370666bfb1dc7655f778621955be2c9159412f63823 |
| desktop-08.jpg | replacements/desktop-08.jpg | 156983 | 61450e831d4ca2c236f3415123717addca02f1a7c8cca595e49c45848b799865 |
| desktop-09.jpg | openvid.dev / desktop-09.jpg | 136765 | cbeba7964d5850f9dc53787069b091fe6b55e5064d607e89fc431e961fc7eccf |
| desktop-10.jpg | replacements/desktop-10.jpg | 582622 | 0c8d34965afc80caef2b1b87016963e2e5bc1305d7212816e66611438aadb7fa |
| desktop-11.jpg | openvid.dev / desktop-11.jpg | 17350 | 55496164efe3f7dceba85b0f7a742007c76bcd95c7da47bf2796b58a631c5919 |
| desktop-12.jpg | replacements/desktop-12.jpg | 255542 | a7b6b1e7920ad48bdc3c5c2f08ddc2d270683c0ae997d41c1128a792aa373063 |
| desktop-13.jpg | puente-de-san-francisco_4k.jpg | 1116810 | 3e562bda256553446d31296ef8d3f3b64b3e35169bee99f33c2ab9ca57e66cc5 |
| desktop-14.jpg | wallhaven-4dl79j.jpg | 781334 | 9a5eeccc2af619fccc3f2c2323fdd22efeb47b9b07a1f4ddde618b3cfb442af7 |
| desktop-15.jpg | wallhaven-72pgxv.jpg | 447496 | 509e5ea32487225983934872d16c5f7759d6e15ff1800fc358f6192c0b8e474a |
| desktop-16.jpg | wallhaven-9mm2mk.jpg | 1659618 | bda1763944c7d81812c0ab8e4ba625bd172e1cce967baa7e91df444f1f86dbc6 |
| desktop-17.jpg | wallhaven-jxv78q.jpg | 773773 | 10034d79cf1ed2fc560d080e647f103de19750483fd70a616a01bd9d2cc7792a |
| desktop-18.jpg | wallhaven-lm38ep.jpg | 1128899 | 017f35b04eb39b997f74957395ec439ae3f583b94df491cfb13c8b39e51212aa |
| desktop-19.jpg | wallhaven-odgmrl.jpg | 1203432 | f1e0922fedac9fd5c060cf2eabaff40cf369ab65f10ea0ffb3db65e91c46a60b |
| desktop-20.jpg | wp7045248-tokyo-night-anime-wallpapers.jpg | 413070 | 37c2fc28a43e784690e9aa35626e42750ad58505fc60b01302df06f8e414aa38 |
| desktop-21.jpg | supplied by maintainer / 2026-11-26 | 2009789 | 521071811482fda9bf72b843f7a3dab5444cbfd53422ffcd028216a1f0eaadf0 |
| desktop-22.jpg | supplied by maintainer / 2026-11-26 | 1649287 | 182c95ad5be67fbbf8e4520d0654cf686f838bd470e1325b31ae7b9f31067aa7 |
| desktop-23.jpg | supplied by maintainer / 2026-11-26 | 489699 | a28ab3d3ebc53f01eb36f1c857956d7de5cea59512e8e1827e45b3c550f82be5 |
| desktop-24.jpg | supplied by maintainer / 2026-11-26 | 963496 | c9db2f8fc150856a0e1c0f802596cdcc5034f6257ac07cb4f83b85815e4e55f6 |

Downloads have a five-second connection timeout and thirty-second total timeout,
accept only the fixed HTTPS origin without redirects, and check exact size and
SHA-256 before publishing the cached file. Failed downloads leave the current
Windows wallpaper and prior cache untouched. No shell commands or executables
are downloaded or launched.

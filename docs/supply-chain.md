# Supply-chain audit — provider-telegram + provider-discord

> Audited 114 unique crates in the dependency closure of `crates/provider-telegram` and `crates/provider-discord` (via `cargo tree`, 2026-08-11). Policy: every crate's crates.io `created_at` must be ≥ 14 days old (`docs/architecture.md` §5, `docs/research/zeroclaw.md` Appendix A). **Result: PASS — 0 failures.** The newest crate is zmij at 236 days.

Workspace-local crates (`provider-core`, `provider-telegram`, `provider-discord`) are excluded — they are not on crates.io.

| Crate               | First published | Age (days) |
| ------------------- | --------------- | ---------- |
| zmij                | 2025-12-18      | 236        |
| serde_core          | 2025-09-13      | 331        |
| find-msvc-tools     | 2025-08-29      | 346        |
| icu_locale_core     | 2024-11-23      | 626        |
| potential_utf       | 2024-11-23      | 626        |
| idna_adapter        | 2024-07-03      | 769        |
| icu_normalizer_data | 2023-09-23      | 1052       |
| icu_properties_data | 2023-09-23      | 1052       |
| zerotrie            | 2023-09-23      | 1053       |
| rustls-pki-types    | 2023-08-31      | 1076       |
| rustls-webpki       | 2023-01-09      | 1309       |
| http-body-util      | 2022-10-25      | 1385       |
| icu_collections     | 2022-08-05      | 1467       |
| utf8_iter           | 2022-04-19      | 1575       |
| zerofrom            | 2022-04-06      | 1588       |
| zerofrom-derive     | 2022-04-06      | 1588       |
| hyper-util          | 2022-01-15      | 1669       |
| zerovec-derive      | 2021-12-11      | 1704       |
| icu_properties      | 2021-11-02      | 1743       |
| unicode-ident       | 2021-10-02      | 1774       |
| yoke-derive         | 2021-07-02      | 1865       |
| yoke                | 2021-05-01      | 1928       |
| icu_normalizer      | 2021-04-29      | 1929       |
| cpufeatures         | 2021-04-26      | 1932       |
| zerovec             | 2021-04-19      | 1939       |
| litemap             | 2021-02-23      | 1995       |
| crypto-common       | 2021-02-10      | 2007       |
| writeable           | 2020-11-12      | 2097       |
| icu_provider        | 2020-10-15      | 2126       |
| form_urlencoded     | 2020-06-19      | 2243       |
| atomic-waker        | 2020-05-29      | 2265       |
| sync_wrapper        | 2020-05-16      | 2277       |
| futures-macro       | 2019-11-06      | 2469       |
| pin-project-lite    | 2019-10-22      | 2485       |
| displaydoc          | 2019-10-10      | 2497       |
| thiserror           | 2019-10-09      | 2497       |
| thiserror-impl      | 2019-10-09      | 2497       |
| tinystr             | 2019-08-10      | 2558       |
| tracing-attributes  | 2019-08-08      | 2559       |
| futures-task        | 2019-07-29      | 2570       |
| async-trait         | 2019-07-23      | 2576       |
| tracing-core        | 2019-06-20      | 2608       |
| tower-layer         | 2019-04-27      | 2663       |
| tokio-macros        | 2019-04-24      | 2666       |
| http-body           | 2019-04-04      | 2685       |
| ppv-lite86          | 2019-02-01      | 2747       |
| getrandom           | 2019-01-19      | 2761       |
| rand_chacha         | 2018-10-17      | 2855       |
| zeroize             | 2018-10-03      | 2869       |
| zerocopy            | 2018-08-15      | 2918       |
| once_cell           | 2018-08-02      | 2931       |
| ryu                 | 2018-07-28      | 2935       |
| try-lock            | 2018-03-15      | 3070       |
| want                | 2018-03-15      | 3070       |
| futures-channel     | 2018-03-05      | 3080       |
| futures-core        | 2018-03-05      | 3080       |
| futures-sink        | 2018-03-05      | 3080       |
| futures-util        | 2018-03-05      | 3080       |
| tower-service       | 2018-02-19      | 3094       |
| tracing             | 2017-11-27      | 3178       |
| rand_core           | 2017-09-14      | 3253       |
| ipnet               | 2017-08-14      | 3283       |
| proc-macro2         | 2017-07-06      | 3323       |
| percent-encoding    | 2017-06-13      | 3345       |
| block-buffer        | 2017-06-12      | 3347       |
| socket2             | 2017-06-07      | 3351       |
| subtle              | 2017-05-31      | 3359       |
| tokio-tungstenite   | 2017-03-17      | 3433       |
| tungstenite         | 2017-03-17      | 3433       |
| tower-http          | 2017-03-10      | 3441       |
| stable_deref_trait  | 2017-03-09      | 3442       |
| tokio-rustls        | 2017-02-22      | 3457       |
| version_check       | 2017-01-15      | 3495       |
| tower               | 2016-12-23      | 3517       |
| reqwest             | 2016-10-16      | 3585       |
| synstructure        | 2016-10-09      | 3592       |
| hyper-rustls        | 2016-10-08      | 3594       |
| digest              | 2016-10-06      | 3596       |
| serde_urlencoded    | 2016-09-11      | 3621       |
| syn                 | 2016-09-07      | 3625       |
| quote               | 2016-09-03      | 3629       |
| serde_derive        | 2016-08-29      | 3634       |
| webpki-roots        | 2016-08-28      | 3635       |
| rustls              | 2016-08-27      | 3636       |
| ring                | 2016-08-15      | 3647       |
| tokio               | 2016-07-01      | 3692       |
| itoa                | 2016-06-25      | 3698       |
| untrusted           | 2016-06-05      | 3718       |
| idna                | 2016-03-27      | 3789       |
| data-encoding       | 2015-12-05      | 3902       |
| base64              | 2015-12-04      | 3903       |
| utf-8               | 2015-10-29      | 3939       |
| generic-array       | 2015-09-27      | 3971       |
| typenum             | 2015-09-26      | 3972       |
| serde_json          | 2015-08-07      | 4021       |
| cfg-if              | 2015-07-08      | 4052       |
| shlex               | 2015-06-22      | 4067       |
| slab                | 2015-06-15      | 4074       |
| memchr              | 2015-06-11      | 4078       |
| smallvec            | 2015-04-06      | 4145       |
| httparse            | 2015-02-20      | 4189       |
| byteorder           | 2015-02-03      | 4206       |
| rand                | 2015-02-03      | 4207       |
| bytes               | 2015-01-30      | 4211       |
| libc                | 2015-01-15      | 4225       |
| bitflags            | 2015-01-15      | 4226       |
| cc                  | 2014-12-16      | 4256       |
| log                 | 2014-12-13      | 4258       |
| serde               | 2014-12-05      | 4266       |
| hyper               | 2014-11-22      | 4280       |
| http                | 2014-11-20      | 4281       |
| sha1                | 2014-11-21      | 4281       |
| url                 | 2014-11-14      | 4287       |
| mio                 | 2014-11-11      | 4290       |

## Notable dependency sources

- `zmij` (newest, 236 d) — pull in by `serde_json` 1.0.151 (transitive).
- `find-msvc-tools` (346 d) — build-dep of `cc`, which builds `ring` (rustls).
- `icu_*`/`zerotrie`/`potential_utf`/`writeable` — `idna` → `url` → `reqwest`/`tokio-tungstenite` (IDNA handling).
- `ring`/`rustls`/`rustls-webpki`/`webpki-roots` — TLS for `reqwest` (rustls-tls) and `tokio-tungstenite` (rustls-tls-webpki-roots).

All direct dependencies are long-established, widely used crates (tokio, reqwest, serde, tokio-tungstenite, futures-util, tracing, async-trait).

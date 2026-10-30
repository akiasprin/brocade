# Site brand icon sources

`site-icon.tsx` embeds only SVG path geometry, with monochrome `currentColor` rendering. No external images, scripts, fonts or favicon services are loaded at runtime. These marks identify observed services; they do not imply endorsement.

- YouTube, Netflix, TikTok, Telegram, OpenAI, Claude, GitHub, Steam, Discord, Spotify, Google and Apple: [Simple Icons 14.15.0](https://github.com/simple-icons/simple-icons/tree/14.15.0/icons). Claude maps to the existing `anthropic` category ID.
- Telegram uses only the paper-plane subpath of the Simple Icons mark, with the surrounding circle removed and the viewBox tightened.
- Tencent uses the QQ penguin silhouette from [Font Awesome Free 6.7.2 qq.svg](https://github.com/FortAwesome/Font-Awesome/blob/6.7.2/svgs/brands/qq.svg), Copyright 2024 Fonticons, Inc., under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Path geometry is unchanged; fill inherits the surrounding text color. See the [Font Awesome license](https://github.com/FortAwesome/Font-Awesome/blob/6.7.2/LICENSE.txt).
- Microsoft: [Simple Icons 11.15.0](https://github.com/simple-icons/simple-icons/blob/11.15.0/icons/microsoft.svg).
- Simple Icons artwork is distributed under [CC0 1.0 Universal](https://github.com/simple-icons/simple-icons/blob/14.15.0/LICENSE.md). Trademark rights are not waived by CC0.
- X / Twitter: [Simple Icons 14.15.0 x.svg](https://github.com/simple-icons/simple-icons/blob/14.15.0/icons/x.svg), under the same CC0 license; the category retains the stable `twitter` ID.
- Facebook: Simple Icons 14.15.0 facebook.svg (same CC0 license). Instagram camera and Meta shared infinity marks are locally drawn monochrome identifiers, not externally loaded assets; trademark rights remain with their owners.
- Disney+: the Disney D monogram from [Disney Stub.svg](https://commons.wikimedia.org/wiki/File:Disney_Stub.svg), original Disney artwork, SVG conversion by FireDragonValo, revision dated 2024-06-04. The source describes the text logo as public domain (PD-textlogo), subject to trademark restrictions. Path geometry is unchanged; the source's red fill is replaced by the surrounding text color.

All marks remain the property of their respective trademark owners. Generic observation categories use Brocade's existing interface icons.

Additional categories: Perplexity, Hugging Face, WhatsApp, Reddit, Twitch, GitLab, Zoom, Slack, LINE, Signal, Speedtest, Bilibili, ByteDance, Baidu, Alibaba.com, Huawei, Xiaomi, Kuaishou and Meituan use Simple Icons 14.15.0 (same CC0 license). Source filenames are their lowercase names without spaces, except Alibaba.com (`alibabadotcom.svg`). Douyin uses the same music-note outline as TikTok. xAI, Cursor, NetEase and JD use locally drawn letter/pointer identifiers; Pinduoduo uses a locally drawn heart identifier. These local identifiers are not reproductions of official brand artwork.

Gemini’s four-point star and Prime Video’s play/curved-arrow identifiers in `panes/node-unlock.tsx` are locally drawn monochrome identifiers, not official brand artwork. No external asset is loaded.

- DAZN: the complete boxed DAZN mark from [Simple Icons 14.15.0 dazn.svg](https://github.com/simple-icons/simple-icons/blob/14.15.0/icons/dazn.svg), under the same CC0 license. All four letters and the frame are retained.
- HBO Max: the complete stacked one-color mark from the [official HBO Max brand guidelines](https://brand.hbomax.com/logos), [HBO_Max_Vert_W_RGB.svg](https://cdn.prod.website-files.com/6911e6b4bcc8af690d496a26/6911e6b4bcc8af690d496df3_HBO_Max_Vert_W_RGB.svg). Original Warner Bros. Discovery / HBO trademark artwork; no CC0 license is asserted. The five paths and H polygon are retained, with the polygon represented as an equivalent closed path. The white fill inherits the surrounding text color for the existing monochrome interface; geometry and proportions are preserved.

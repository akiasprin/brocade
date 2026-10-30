#!/usr/bin/env python3
"""Regenerate unlock flags from published data; no hand-maintained POP table.

Run from any directory. --source-dir caches the downloaded inputs for offline
reproduction. The generated JSON records each input URL and content digest.
Only this developer tool uses the network; the product uses bundled data.
"""
import argparse
import csv
import hashlib
import html
import io
import json
import math
from pathlib import Path
import re
import tarfile
from urllib.request import urlopen

SOURCES = {
    "network.txt": (
        "https://www.fastly.com/documentation/guides/getting-started/concepts/using-fastlys-global-pop-network/",
        None,
    ),
    "countries.geojson": (
        "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_10m_admin_0_countries.geojson",
        "239eec57ac17f100a11e2536cffc56752c318b50ae765b0918ff7aab4ce8f255",
    ),
    "airports.csv": (
        "https://davidmegginson.github.io/ourairports-data/airports.csv",
        None,
    ),
    "iso-package.tgz": (
        "https://registry.npmjs.org/i18n-iso-countries/-/i18n-iso-countries-7.14.0.tgz",
        "bb418ebfb965789fcfbcce7dee59888d901ef64b03601da78b2e224cd6f5825c",
    ),
}


def inside(x, y, ring):
    result = False
    x1, y1 = ring[-1][:2]
    for point in ring:
        x2, y2 = point[:2]
        if (y1 > y) != (y2 > y) and x < (x2 - x1) * (y - y1) / (y2 - y1) + x1:
            result = not result
        x1, y1 = x2, y2
    return result


def distance(lat1, lon1, lat2, lon2):
    value = (
        math.sin(math.radians(lat2 - lat1) / 2) ** 2
        + math.cos(math.radians(lat1))
        * math.cos(math.radians(lat2))
        * math.sin(math.radians(lon2 - lon1) / 2) ** 2
    )
    return 6371 * 2 * math.asin(min(1, math.sqrt(value)))


def generate(raw):
    with tarfile.open(fileobj=io.BytesIO(raw["iso-package.tgz"]), mode="r:gz") as archive:
        codes = json.load(archive.extractfile("package/codes.json"))
    iso3 = {row[1]: row[0] for row in codes}
    regions = set(iso3.values())
    assert len(iso3) == len(codes) >= 249 and all(re.fullmatch("[A-Z]{3}", code) for code in iso3)
    polygons = []
    for feature in json.loads(raw["countries.geojson"])["features"]:
        region = feature["properties"]["ISO_A2_EH"]
        if region not in regions:
            continue
        geometry = feature["geometry"]
        parts = geometry["coordinates"] if geometry["type"] == "MultiPolygon" else [geometry["coordinates"]]
        for rings in parts:
            xs, ys = zip(*(point[:2] for point in rings[0]))
            polygons.append((region, (min(xs), min(ys), max(xs), max(ys)), rings))
    airports = []
    for row in csv.DictReader(io.StringIO(raw["airports.csv"].decode("utf-8"))):
        if row["iso_country"] in regions:
            try:
                airports.append((row["iso_country"], float(row["latitude_deg"]), float(row["longitude_deg"])))
            except ValueError:
                continue
    assert len(polygons) > 1000 and len(airports) > 10000
    pops = {}
    for tr in re.findall(r"<tr[^>]*>(.*?)</tr>", raw["network.txt"].decode("utf-8"), re.S):
        cells = [html.unescape(re.sub("<[^>]+>", "", td)).strip()
                 for td in re.findall(r"<t[dh][^>]*>(.*?)</t[dh]>", tr, re.S)]
        if (len(cells) < 3 or not re.fullmatch("[A-Z]{3}", cells[1])
                or not re.fullmatch(r"-?[\d.]+, -?[\d.]+", cells[2])):
            continue
        code = cells[1]
        assert code not in pops, "duplicate Fastly POP"
        pops[code] = tuple(map(float, cells[2].split(",")))
    assert len(pops) >= 100, "Fastly location table missing or changed"
    fastly, evidence, unmapped = {}, {}, []
    for code, (lat, lon) in sorted(pops.items()):
        matches = {
            region for region, (x1, y1, x2, y2), rings in polygons
            if x1 <= lon <= x2 and y1 <= lat <= y2 and inside(lon, lat, rings[0])
            and not any(inside(lon, lat, hole) for hole in rings[1:])
        }
        method = "natural-earth-polygon"
        # Airport POPs can be on reclaimed land absent from the country outline.
        # Confirm with all OurAirports sites within 5 km, requiring one country.
        # Ambiguous country polygons never fall back to the nearest airport.
        if not matches:
            matches = {
                region for region, a, b in airports
                if abs(a - lat) < 0.05 and abs(b - lon) * math.cos(math.radians(lat)) < 0.05
                and distance(lat, lon, a, b) <= 5
            }
            method = "ourairports-within-5km-unanimous-country"
        if len(matches) == 1:
            fastly[code] = next(iter(matches))
            evidence[code] = {"coordinates": [lat, lon], "method": method}
        else:
            unmapped.append(code)
    return {"iso3": dict(sorted(iso3.items())), "fastly": fastly}, evidence, unmapped, len(pops)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    args = parser.parse_args()
    args.source_dir.mkdir(parents=True, exist_ok=True)
    raw, provenance = {}, {}
    for name, (url, pinned) in SOURCES.items():
        path = args.source_dir / name
        if not path.exists():
            with urlopen(url, timeout=30) as response:
                data = response.read(20_000_001)
            assert len(data) <= 20_000_000, "source exceeds size limit"
            path.write_bytes(data)
        raw[name] = path.read_bytes()
        digest = hashlib.sha256(raw[name]).hexdigest()
        assert pinned is None or digest == pinned, name + " content digest mismatch"
        provenance[name] = {"url": url, "sha256": digest}
    mapping, evidence, unmapped, total = generate(raw)
    ui = Path(__file__).resolve().parents[1] / "src/ui"
    (ui / "unlock-region-data.json").write_text(json.dumps(mapping, indent=2) + "\n")
    Path(__file__).with_suffix(".sources.json").write_text(json.dumps({
        "sources": provenance, "fastly_pops": total, "mapped_pops": len(mapping["fastly"]),
        "unmapped_pops": unmapped, "evidence": evidence,
    }, indent=2) + "\n")
    print(json.dumps({"iso3_codes": len(mapping["iso3"]), "fastly_pops": total,
                      "mapped_pops": len(mapping["fastly"]), "unmapped_pops": unmapped}))


if __name__ == "__main__":
    main()

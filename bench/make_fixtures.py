"""Generates bench/fixtures: ten ordinary pages (article text, CSS, PNG images, a little script)
served from loopback by bench/memory.ps1, so memory runs never depend on live sites.
Deterministic: the same files on every run. Usage: python -I bench/make_fixtures.py"""
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures")


def png(path, w, h, seed):
    # A smooth gradient with a little texture: compresses like a photo-ish image, not like a flat fill.
    rows = bytearray()
    for y in range(h):
        rows.append(0)
        for x in range(w):
            n = ((x * 7 + y * 13 + seed * 31) * 2654435761 >> 24) & 15
            rows += bytes(((x * 255 // w + n) & 255, (y * 255 // h + seed * 40) & 255, ((x + y + seed * 50) & 255) ^ n))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(bytes(rows), 6)))
        f.write(chunk(b"IEND", b""))


CSS = """*{box-sizing:border-box}body{margin:0;font:16px/1.6 system-ui,Segoe UI,sans-serif;color:#222;background:#fafafa}
header{position:sticky;top:0;background:#123;color:#fff;padding:12px 24px;display:flex;gap:16px}header a{color:#cde}
main{max-width:960px;margin:24px auto;padding:0 16px;display:grid;grid-template-columns:1fr 260px;gap:24px}
article img{width:100%;height:auto;border-radius:8px}aside{background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px}
table{border-collapse:collapse;width:100%}td,th{border:1px solid #ccc;padding:4px 8px}tr:nth-child(odd){background:#f2f2f2}
.card{display:inline-block;width:30%;margin:1%;padding:8px;background:#fff;box-shadow:0 1px 3px #0003;border-radius:6px}
footer{padding:24px;text-align:center;color:#666}button{padding:6px 12px}"""

WORDS = ("velocity engine memory layout render page browser window cache image style script network "
         "token parser tree frame paint scroll text font event queue thread process tab view").split()


def para(seed, n=90):
    return " ".join(WORDS[(seed * 7 + i * 11 + i * i) % len(WORDS)] for i in range(n)).capitalize() + "."


def page(i):
    imgs = "".join(f'<figure><img src="img/p{i}_{k}.png" width="800" height="450" alt=""><figcaption>Figure {k}</figcaption></figure>'
                   f"<p>{para(i * 10 + k)}</p><p>{para(i * 10 + k + 5)}</p>" for k in range(4))
    rows = "".join(f"<tr><td>{r}</td><td>{WORDS[(r + i) % len(WORDS)]}</td><td>{r * i * 37 % 1000}</td></tr>" for r in range(40))
    cards = "".join(f'<div class="card"><b>{WORDS[(c * 3 + i) % len(WORDS)]}</b><p>{para(c + i, 20)}</p></div>' for c in range(12))
    nav = "".join(f'<a href="page{j}.html">Page {j}</a>' for j in range(1, 11))
    script = """<script>
const list = document.getElementById('list');
for (let k = 0; k < 300; k++) { const li = document.createElement('li'); li.textContent = 'Item ' + k; list.appendChild(li); }
document.getElementById('more').onclick = () => { for (let k = 0; k < 50; k++) list.appendChild(document.createElement('li')).textContent = 'More ' + k; };
let t = 0; setInterval(() => { document.getElementById('clock').textContent = 'Uptime ' + (++t) + ' s'; }, 1000);
</script>"""
    return (f"<!doctype html><html lang=en><head><meta charset=utf-8><title>Fixture page {i}</title>"
            f'<link rel=stylesheet href="style.css"></head><body><header><b>Fixture {i}</b>{nav}<span id=clock></span></header>'
            f"<main><article><h1>Fixture page {i}</h1><p>{para(i)}</p>{imgs}<h2>Data</h2><table>{rows}</table>{cards}</article>"
            f'<aside><h3>Sidebar</h3><button id=more>More</button><ul id=list></ul><form><input placeholder="Search"><select>'
            + "".join(f"<option>{w}</option>" for w in WORDS[:8]) +
            f"</select></form></aside></main><footer>Fixture footer</footer>{script}</body></html>")


def main():
    os.makedirs(os.path.join(OUT, "img"), exist_ok=True)
    with open(os.path.join(OUT, "style.css"), "w") as f:
        f.write(CSS)
    for i in range(1, 11):
        with open(os.path.join(OUT, f"page{i}.html"), "w", encoding="utf-8") as f:
            f.write(page(i))
        for k in range(4):
            path = os.path.join(OUT, "img", f"p{i}_{k}.png")
            if not os.path.exists(path):
                png(path, 800, 450, i * 4 + k)
    assert len([n for n in os.listdir(OUT) if n.startswith("page")]) == 10
    print("fixtures in", OUT)


if __name__ == "__main__":
    main()

from pathlib import Path
from PIL import Image, ImageDraw, ImageFont


ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "public" / "dopedb-dashboard.png"
W, H = 1600, 1120


def font(size: int, weight: str = "regular") -> ImageFont.FreeTypeFont:
    candidates = [
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/Library/Fonts/Arial.ttf",
    ]
    if weight == "bold":
        candidates = [
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/Library/Fonts/Arial Bold.ttf",
            *candidates,
        ]
    for candidate in candidates:
        try:
            return ImageFont.truetype(candidate, size)
        except OSError:
            continue
    return ImageFont.load_default()


def rr(draw: ImageDraw.ImageDraw, box, radius, fill, outline=None, width=1):
    draw.rounded_rectangle(box, radius=radius, fill=fill, outline=outline, width=width)


def text(draw: ImageDraw.ImageDraw, xy, value, fill, size=28, weight="regular"):
    draw.text(xy, value, fill=fill, font=font(size, weight))


img = Image.new("RGB", (W, H), "#11130f")
draw = ImageDraw.Draw(img)

# Subtle window glow and app chrome.
draw.rectangle((0, 0, W, H), fill="#11130f")
for i in range(0, W, 64):
    draw.line((i, 0, i, H), fill="#181b16", width=1)
for i in range(0, H, 64):
    draw.line((0, i, W, i), fill="#181b16", width=1)

margin = 54
rr(draw, (margin, margin, W - margin, H - margin), 26, "#f7f8f3")
rr(draw, (margin, margin, W - margin, margin + 76), 26, "#e9eee4")
draw.rectangle((margin, margin + 40, W - margin, margin + 76), fill="#e9eee4")
for idx, color in enumerate(["#ff6b5c", "#f2b84b", "#6fd26c"]):
    draw.ellipse((margin + 28 + idx * 28, margin + 28, margin + 42 + idx * 28, margin + 42), fill=color)
text(draw, (margin + 130, margin + 24), "DopeDB", "#11130f", 26, "bold")
text(draw, (W - margin - 318, margin + 24), "Local audit enabled", "#1d7b45", 24, "bold")

content_top = margin + 76
sidebar_w = 306
draw.rectangle((margin, content_top, margin + sidebar_w, H - margin), fill="#f0f3eb")
draw.line((margin + sidebar_w, content_top, margin + sidebar_w, H - margin), fill="#d4dacd", width=2)

text(draw, (margin + 30, content_top + 34), "Connections", "#42463d", 24, "bold")
profiles = [
    ("prod-readonly", "PostgreSQL", "#1d7b45"),
    ("stage-write-gated", "MySQL", "#d26152"),
    ("local-fixtures", "SQLite", "#4a95b8"),
]
y = content_top + 86
for name, db, color in profiles:
    rr(draw, (margin + 22, y, margin + sidebar_w - 22, y + 78), 14, "#ffffff", "#d8ddd2", 2)
    draw.ellipse((margin + 42, y + 24, margin + 72, y + 54), fill=color)
    text(draw, (margin + 86, y + 18), name, "#11130f", 22, "bold")
    text(draw, (margin + 86, y + 46), db, "#697064", 18)
    y += 92

text(draw, (margin + 30, y + 26), "Tables", "#697064", 18, "bold")
for idx, label in enumerate(["customers", "orders", "audit_events", "migrations"]):
    yy = y + 72 + idx * 48
    text(draw, (margin + 44, yy), label, "#42463d", 21)

main_x = margin + sidebar_w
main_y = content_top
draw.rectangle((main_x, main_y, W - margin, H - margin), fill="#fbfbf7")

tabs_y = main_y + 22
tabs = ["Data", "SQL", "History", "Audit", "Agent"]
tx = main_x + 36
for tab in tabs:
    tw = 34 + len(tab) * 14
    fill = "#11130f" if tab == "Agent" else "#ffffff"
    color = "#ffffff" if tab == "Agent" else "#42463d"
    rr(draw, (tx, tabs_y, tx + tw, tabs_y + 42), 12, fill, "#d8ddd2", 1)
    text(draw, (tx + 18, tabs_y + 10), tab, color, 18, "bold")
    tx += tw + 12

text(draw, (main_x + 36, main_y + 94), "Ask your database", "#11130f", 38, "bold")
text(draw, (main_x + 36, main_y + 142), "Codex proposes SQL. DopeDB keeps the keys and gates execution.", "#697064", 23)

panel_x = main_x + 36
panel_y = main_y + 204
panel_w = 706
rr(draw, (panel_x, panel_y, panel_x + panel_w, panel_y + 274), 16, "#11130f")
text(draw, (panel_x + 28, panel_y + 26), "codex proposal", "#6fd26c", 20, "bold")
code = [
    "SELECT customer_id, plan, mrr",
    "FROM customers",
    "WHERE renewal_at < now() + interval '14 days'",
    "ORDER BY mrr DESC",
    "LIMIT 25;",
]
cy = panel_y + 72
for line in code:
    text(draw, (panel_x + 28, cy), line, "#f4f7f0", 25)
    cy += 38

approval_x = panel_x + panel_w + 26
approval_w = W - margin - approval_x - 36
rr(draw, (approval_x, panel_y, approval_x + approval_w, panel_y + 274), 16, "#ffffff", "#d8ddd2", 2)
text(draw, (approval_x + 26, panel_y + 26), "Safety gate", "#11130f", 28, "bold")
badges = [("READ", "#1d7b45"), ("25 rows", "#4a95b8"), ("no secrets", "#d26152")]
bx = approval_x + 26
for label, color in badges:
    rr(draw, (bx, panel_y + 76, bx + 112, panel_y + 112), 10, color)
    text(draw, (bx + 16, panel_y + 84), label, "#ffffff", 17, "bold")
    bx += 126
text(draw, (approval_x + 26, panel_y + 144), "Auto-run allowed by read-only policy.", "#42463d", 24)
rr(draw, (approval_x + 26, panel_y + 202, approval_x + 196, panel_y + 244), 10, "#11130f")
text(draw, (approval_x + 52, panel_y + 213), "Run query", "#ffffff", 19, "bold")

table_y = panel_y + 310
table_h = 270
rr(draw, (panel_x, table_y, W - margin - 36, table_y + table_h), 16, "#ffffff", "#d8ddd2", 2)
headers = ["customer_id", "plan", "mrr", "renewal_at", "risk"]
col_x = [panel_x + 28, panel_x + 226, panel_x + 404, panel_x + 568, panel_x + 808]
for x, header in zip(col_x, headers):
    text(draw, (x, table_y + 28), header, "#697064", 18, "bold")
draw.line((panel_x, table_y + 68, W - margin - 36, table_y + 68), fill="#d8ddd2", width=2)
rows = [
    ("1842", "pro", "$12,400", "2026-07-18", "low"),
    ("0927", "team", "$8,160", "2026-07-19", "low"),
    ("5011", "trial", "$0", "2026-07-20", "review"),
    ("7730", "pro", "$6,920", "2026-07-21", "low"),
]
ry = table_y + 86
for idx, row in enumerate(rows):
    if idx % 2 == 1:
        draw.rectangle((panel_x + 2, ry - 14, W - margin - 38, ry + 34), fill="#f7f8f3")
    for x, value in zip(col_x, row):
        color = "#1d7b45" if value == "low" else "#d26152" if value == "review" else "#11130f"
        text(draw, (x, ry), value, color, 20, "bold" if value in ["low", "review"] else "regular")
    ry += 48

audit_y = table_y + table_h + 26
rr(draw, (panel_x, audit_y, W - margin - 36, H - margin - 30), 16, "#e9eee4", "#d8ddd2", 2)
text(draw, (panel_x + 26, audit_y + 24), "Audit trail", "#11130f", 25, "bold")
events = ["parsed as read", "policy check passed", "result exported", "hash chained"]
ex = panel_x + 180
for event in events:
    draw.ellipse((ex, audit_y + 30, ex + 14, audit_y + 44), fill="#6fd26c")
    text(draw, (ex + 24, audit_y + 24), event, "#42463d", 20)
    ex += 210

OUT.parent.mkdir(parents=True, exist_ok=True)
img.save(OUT, quality=95)
print(OUT)

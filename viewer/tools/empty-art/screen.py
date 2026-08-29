"""Shaded solids -> a scalar tone field -> a halftone screen.

The screen is the one from the Astrolabe landing hero
(`astrolabe-landing/src/DotField.jsx`): a fixed lattice, one round mark per
cell, radius AND alpha both driven by the field. This module only produces the
field; the ink ramp lives in the component, because it has to be resolved
against whatever colour the page is actually using.

`draw()` renders a preview raster so a change can be looked at without a
browser. It is not what ships — the browser paints from the packed field.
"""
import math
from PIL import Image, ImageDraw
import solids

CELL, MAX_R = 2.5, 1.0
COLS, ROWS = 72, 52
BOXW, BOXH = COLS * CELL, ROWS * CELL
SS = 6
TARGET, MAXW, MAXH = 92.0, 148.0, 104.0

def fit(faces):
    pts = [q for poly, _ in faces for q in poly]
    x0 = min(q[0] for q in pts); x1 = max(q[0] for q in pts)
    y0 = min(q[1] for q in pts); y1 = max(q[1] for q in pts)
    w, h = x1 - x0, y1 - y0
    k = min(TARGET / math.sqrt(w * h), MAXW / w, MAXH / h)
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    return [([((qx - cx) * k + BOXW / 2, (qy - cy) * k + BOXH / 2) for qx, qy in poly], sh)
            for poly, sh in faces], w * k, h * k

# The exposure. Flat shades come out of the model at 0.4-0.95; printed at that
# level every cell is a full dot and the lattice closes into a solid texture. A
# screen wants most of its area OPEN, with tone earned back by the light — so
# the shading is scaled down and a single soft lamp, set just off the object's
# upper left, is what lifts anything to the top of the range.
EXPOSURE = 1.0
AMBIENT = 0.74          # how much of a face's tone survives with no lamp on it
LAMP_X, LAMP_Y = 0.36, 0.26
LAMP_R = 0.95
# Contrast, not exposure. Pulling the whole field down flattens it; a gamma
# above 1 keeps the lit faces at the top of the range and drops the turned-away
# ones toward nothing, which is what opens the lattice where the object is dark.
GAMMA_TONE = 1.12
# Every face boundary prints at the top of the range. Tone alone cannot hold a
# form at this ruling — a turned-away cheek is only a handful of sparse dots,
# and without a contour the object dissolves into its own shadow. Tone carries
# the volume; the contour carries the shape.
EDGE = 0.96

def tone_field(name, edge=True):
    faces, w, h = fit(solids.ART[name]())
    img = Image.new("L", (int(BOXW * SS), int(BOXH * SS)), 0)
    d = ImageDraw.Draw(img)
    for poly, sh in faces:
        pts = [(x * SS, y * SS) for x, y in poly]
        v = int(max(0.0, min(1.0, sh)) * 255)
        d.polygon(pts, fill=v)
        # A hairline at full light along every face boundary. On a screen this
        # is what keeps a dark cheek from dissolving into its neighbour: the
        # edge is the only place tone changes fast enough to survive the ruling.
        if edge:
            d.line(pts + [pts[0]], fill=int(EDGE * 255), width=max(1, int(SS * 0.5)))
    small = img.resize((COLS, ROWS), Image.BOX)
    px = small.load()
    lx, ly = LAMP_X * COLS, LAMP_Y * ROWS
    rad = LAMP_R * max(COLS, ROWS)
    field = []
    for j in range(ROWS):
        row = []
        for i in range(COLS):
            d = math.hypot(i - lx, (j - ly) * 1.35) / rad
            lamp = max(0.0, 1.0 - d * d)
            v = (px[i, j] / 255.0) * EXPOSURE * (AMBIENT + (1 - AMBIENT) * lamp)
            row.append(min(1.0, v) ** GAMMA_TONE)
        field.append(row)
    return field, w, h

COOL=(174,156,220); INK=(235,215,224); GOLD=(255,176,96); HOT=(255,249,238)
def smoothstep(a,b,x):
    t=max(0.0,min(1.0,(x-a)/(b-a))); return t*t*(3-2*t)
def ink(tone, warm):
    knee=0.44
    if warm<knee:
        k=warm/knee; c=[COOL[i]+(INK[i]-COOL[i])*k for i in range(3)]
    else:
        k=(warm-knee)/(1-knee); c=[INK[i]+(GOLD[i]-INK[i])*k for i in range(3)]
    burn=smoothstep(0.74,1,tone)*(0.30+0.55*warm)
    return tuple(int(c[i]+(HOT[i]-c[i])*burn) for i in range(3))

def draw(field, S=3, bg=(16,16,18), alpha_gamma=0.78):
    tile=Image.new("RGB",(int(BOXW*S),int(BOXH*S)),bg)
    dr=ImageDraw.Draw(tile,"RGBA")
    for j in range(ROWS):
        for i in range(COLS):
            v=field[j][i]
            if v<=0.02: continue
            r=MAX_R*v*S
            cx=(i+0.5)*CELL*S; cy=(j+0.5)*CELL*S
            a=int((v**alpha_gamma)*255)
            warm=smoothstep(0.42,1.0,v)*0.92
            dr.ellipse([cx-r,cy-r,cx+r,cy+r], fill=ink(v,warm)+(a,))
    return tile.resize((int(BOXW),int(BOXH)),Image.LANCZOS)

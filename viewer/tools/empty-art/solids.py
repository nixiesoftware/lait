"""The ten objects as SHADED SOLIDS in one isometric space.

The line version traced silhouettes; a screen has nothing to do with a
silhouette. Here every object is a set of faces, each carrying a shade from one
light (upper-left, in front), and the field the halftone screens is that shading
— so the dots thin out across a face the way a printing screen renders a tone,
and the drawing has a lit side and a dark side rather than a uniform wire.

Faces are emitted back-to-front; the renderer paints them in order.
"""
import math

U = 11.0
CX, CY = 0.0, 0.0

# One light. Top faces take it flat, the near-left cheek takes it at a slant,
# the near-right cheek is turned away. These three values are the whole
# lighting model and they are what makes an iso box read as a box.
# One light, given as a direction, so a curved surface and a flat cheek are
# shaded by the same rule instead of two rules that happen to agree on boxes.
LX, LY, LZ = 0.28, 0.55, 0.79

def lit(nx, ny, nz):
    d = LX * nx + LY * ny + LZ * nz
    return max(0.0, min(1.0, 0.12 + 1.02 * d))

TOP, LEFT, RIGHT = lit(0, 0, 1), lit(0, 1, 0), lit(1, 0, 0)

def p(x, y, z=0.0):
    return (CX + (x - y) * 2 * U, CY + (x + y) * U - z * U)

def rot(pts, a):
    c, s = math.cos(a), math.sin(a)
    return [(x * c - y * s, x * s + y * c) for x, y in pts]

def rect(hx, hy, cx=0.0, cy=0.0):
    return [(cx - hx, cy - hy), (cx + hx, cy - hy), (cx + hx, cy + hy), (cx - hx, cy + hy)]

def top(pts2, z, shade=TOP):
    return ([p(x, y, z) for x, y in pts2], shade)

def wall(a, b, z0, z1, shade):
    return ([p(*a, z0), p(*b, z0), p(*b, z1), p(*a, z1)], shade)

def prism(pts2, z0, z1, shade_top=TOP):
    """A closed footprint extruded upward. Only the two near cheeks are drawn:
    which ones those are follows from the projection, not from a guess."""
    out = []
    n = len(pts2)
    for i in range(n):
        a, b = pts2[i], pts2[(i + 1) % n]
        mx, my = (a[0] + b[0]) / 2, (a[1] + b[1]) / 2
        ex, ey = b[0] - a[0], b[1] - a[1]
        # outward normal of this edge in the ground plane
        nx, ny = ey, -ex
        if nx + ny <= 0:            # facing away from the viewer
            continue
def prism_faces(pts2, z0, z1, shade_top=TOP):
    out = []
    n = len(pts2)
    for i in range(n):
        a, b = pts2[i], pts2[(i + 1) % n]
        ex, ey = b[0] - a[0], b[1] - a[1]
        nx, ny = ey, -ex
        if nx + ny <= 0:
            continue
        m = math.hypot(nx, ny) or 1.0
        out.append((wall(a, b, z0, z1, lit(nx / m, ny / m, 0.0)), (a[0]+b[0]+a[1]+b[1]) / 2))
    out.sort(key=lambda e: e[1])
    return [f for f, _ in out] + [top(pts2, z1, shade_top)]

def ring_pts(r, n, cx=0.0, cy=0.0, t0=0.0, t1=2 * math.pi):
    return [(cx + r * math.cos(t0 + (t1 - t0) * i / n), cy + r * math.sin(t0 + (t1 - t0) * i / n))
            for i in range(n)]


def sphere(cx, cy, cz, r, n=13):
    """A ball. It projects to a circle, so the modelling is entirely in the
    shading: concentric shells, brightest off toward the lamp."""
    out = []
    ox, oy = -0.34, -0.42          # where the highlight sits, in screen units
    scr = p(cx, cy, cz)
    for i in range(n, 0, -1):
        k = i / n
        rr = r * k * 4 * U * 0.5
        hx = scr[0] + ox * r * 2 * U * (1 - k) * 1.15
        hy = scr[1] + oy * r * 2 * U * (1 - k) * 1.15
        sh = 0.18 + 0.82 * (1 - k) ** 0.85
        out.append(([(hx + rr * math.cos(2 * math.pi * j / 30),
                      hy + rr * math.sin(2 * math.pi * j / 30)) for j in range(30)], sh))
    return out

def drum(cx, cy, z0, z1, r, n=34, cap=True):
    """An upright cylinder."""
    out = []
    for i in range(n):
        t0 = 2 * math.pi * i / n
        t1 = 2 * math.pi * (i + 1) / n
        mid = (t0 + t1) / 2
        nx, ny = math.cos(mid), math.sin(mid)
        if nx + ny <= -0.30:
            continue
        out.append((([p(cx + r * math.cos(t0), cy + r * math.sin(t0), z0),
                      p(cx + r * math.cos(t1), cy + r * math.sin(t1), z0),
                      p(cx + r * math.cos(t1), cy + r * math.sin(t1), z1),
                      p(cx + r * math.cos(t0), cy + r * math.sin(t0), z1)]), lit(nx, ny, 0.0)))
    if cap:
        out.append(([p(cx + r * math.cos(2 * math.pi * i / n),
                       cy + r * math.sin(2 * math.pi * i / n), z1) for i in range(n)], TOP))
    return out

def layered(pts2, z0, z1, count):
    """Thin bands cut into the visible cheeks of a block, so a stack reads as a
    stack rather than as one solid slab."""
    out = []
    n = len(pts2)
    for i in range(n):
        a, b = pts2[i], pts2[(i + 1) % n]
        ex, ey = b[0] - a[0], b[1] - a[1]
        nx, ny = ey, -ex
        if nx + ny <= 0:
            continue
        for k in range(1, count):
            z = z0 + (z1 - z0) * k / count
            out.append((wall(a, b, z - 0.035, z + 0.035, 0.10), (a[0]+b[0]+a[1]+b[1]) / 2))
    out.sort(key=lambda e: e[1])
    return [f for f, _ in out]

# ------------------------------------------------------------------ objects

def inbox():
    """A hopper you can see into: tall enough that the walls are the subject."""
    s, h, t, floor = 0.80, 1.22, 0.15, 0.24
    ix = s - t
    f = prism_faces(rect(s, s), 0.0, h, 0.0)[:-1]
    f.append(top(rect(ix, ix), floor, 0.16))
    f.append(wall((-ix, -ix), (ix, -ix), floor, h, 0.44))
    f.append(wall((ix, -ix), (ix, ix), floor, h, 0.34))
    for a, b in (((-s, -s), (s, -s)), ((s, -s), (s, s)), ((s, s), (-s, s)), ((-s, s), (-s, -s))):
        ia = (a[0] * ix / s, a[1] * ix / s)
        ib = (b[0] * ix / s, b[1] * ix / s)
        f.append(([p(*a, h), p(*b, h), p(*ib, h), p(*ia, h)], TOP))
    return f

def archive():
    """A tall lidded crate."""
    s, body_h, lip = 0.72, 1.66, 0.26
    ls = s + 0.11
    return (prism_faces(rect(s, s), 0.0, body_h, 0.0)
            + prism_faces(rect(ls, ls), body_h, body_h + lip, TOP))

def activity():
    """A paper roll with a sheet unspooled."""
    r, L, n = 0.66, 0.86, 40
    f = []
    for i in range(n):
        t0 = 2 * math.pi * i / n
        t1 = 2 * math.pi * (i + 1) / n
        mid = (t0 + t1) / 2
        ny, nz = math.cos(mid), math.sin(mid)
        if 0.45 * ny + 0.62 * nz <= -0.42:
            continue
        f.append(([p(-L, r * math.cos(t0), r + r * math.sin(t0)),
                   p(L, r * math.cos(t0), r + r * math.sin(t0)),
                   p(L, r * math.cos(t1), r + r * math.sin(t1)),
                   p(-L, r * math.cos(t1), r + r * math.sin(t1))], lit(0.0, ny, nz)))
    f.append(([p(L, r * math.cos(2 * math.pi * i / n), r + r * math.sin(2 * math.pi * i / n))
               for i in range(n)], 0.30))
    steps = 20
    def sheet(t):
        return r * 0.10 + t * 1.85, max(r * (1 - t) ** 2 * 1.45, 0.0)
    for i in range(steps):
        y0, z0 = sheet(i / steps)
        y1, z1 = sheet((i + 1) / steps)
        f.append(([p(-L + 0.06, y0, z0), p(L - 0.06, y0, z0),
                   p(L - 0.06, y1, z1), p(-L + 0.06, y1, z1)], TOP - 0.30 * (i / steps)))
    return f

def specs():
    """A ream — a thick pad of paper — with a pen laid across it."""
    hx, hy, h = 0.86, 0.62, 1.05
    foot = rect(hx, hy)
    f = prism_faces(foot, 0.0, h, TOP) + layered(foot, 0.0, h, 6)
    a, half, w = 0.66, 0.80, 0.085
    ax, ay = math.cos(a), math.sin(a)
    nx, ny = -ay, ax
    pen = [(-ax * half + nx * w, -ay * half + ny * w),
           (ax * half + nx * w, ay * half + ny * w),
           (ax * (half + 0.24), ay * (half + 0.24)),
           (ax * half - nx * w, ay * half - ny * w),
           (-ax * half - nx * w, -ay * half - ny * w)]
    return f + prism_faces(pen, h, h + 0.15, TOP)

def issues():
    """A block of cards, its layers cut into the cheeks."""
    hx, hy, h = 0.62, 0.80, 1.30
    foot = rect(hx, hy)
    f = prism_faces(foot, 0.0, h, TOP) + layered(foot, 0.0, h, 7)
    f.append(top(rect(0.30, 0.10, 0.0, -hy + 0.24), h + 0.004, 0.12))
    return f

def people():
    """Two pieces on a board: a body and a head each, the oldest way to draw a
    person with no face."""
    f = []
    for cx, cy, k in ((-0.34, 0.34, 1.0), (0.44, -0.30, 0.82)):
        f += drum(cx, cy, 0.0, 0.30 * k, 0.42 * k)          # foot
        f += drum(cx, cy, 0.28 * k, 1.02 * k, 0.27 * k)     # body
        f += sphere(cx, cy, 1.02 * k + 0.30 * k, 0.30 * k)  # head
    return f

def projects():
    """Three cubes, set apart so they read as separable units of work."""
    f = []
    s, h = 0.40, 1.02
    for cx, cy in ((-0.55, -0.55), (-0.45, 0.85), (0.85, -0.45)):
        f += prism_faces(rect(s, s, cx, cy), 0.0, h, TOP)
    return f

def space():
    """A house: somewhere the work lives."""
    hx, hy = 0.78, 0.66
    wall_h, ridge, over = 1.46, 2.24, 0.12
    ex, ey = hx + over, hy + over
    f = prism_faces(rect(hx, hy), 0.0, wall_h, 0.0)
    f.append(([p(ex, -ey, wall_h), p(ex, ey, wall_h), p(ex, 0, ridge)], lit(1, 0, 0.10)))
    f.append(([p(-ex, 0, ridge), p(ex, 0, ridge),
               p(ex, ey, wall_h), p(-ex, ey, wall_h)], lit(0, 0.62, 0.78)))
    f.append(([p(-0.30, hy, 0), p(0.20, hy, 0), p(0.20, hy, 0.82), p(-0.30, hy, 0.82)], 0.16))
    f.append(([p(hx, -0.26, 0.72), p(hx, 0.22, 0.72), p(hx, 0.22, 1.14), p(hx, -0.26, 1.14)], 0.14))
    return f

def filtered():
    """A funnel with nothing under it. You look into the mouth, so the far
    inside wall is lit and the near inside wall is the one in shadow."""
    rt, zt = 0.82, 2.14
    rs, zs, zb = 0.19, 0.68, 0.02
    n = 44
    f = []
    for i in range(n):                                   # the inside, first
        t0 = 2 * math.pi * i / n
        t1 = 2 * math.pi * (i + 1) / n
        mid = (t0 + t1) / 2
        nx, ny = -math.cos(mid), -math.sin(mid)          # normals point inward
        f.append(([p(rt * math.cos(t0), rt * math.sin(t0), zt),
                   p(rt * math.cos(t1), rt * math.sin(t1), zt),
                   p(rs * math.cos(t1), rs * math.sin(t1), zs),
                   p(rs * math.cos(t0), rs * math.sin(t0), zs)],
                  lit(nx * 0.82, ny * 0.82, -0.57) * 0.66))
    for i in range(n):                                   # then the outside
        t0 = 2 * math.pi * i / n
        t1 = 2 * math.pi * (i + 1) / n
        mid = (t0 + t1) / 2
        nx, ny = math.cos(mid), math.sin(mid)
        if nx + ny <= -0.10:
            continue
        f.append(([p(rt * math.cos(t0), rt * math.sin(t0), zt),
                   p(rt * math.cos(t1), rt * math.sin(t1), zt),
                   p(rs * math.cos(t1), rs * math.sin(t1), zs),
                   p(rs * math.cos(t0), rs * math.sin(t0), zs)], lit(nx * 0.82, ny * 0.82, 0.57)))
    f += drum(0.0, 0.0, zb, zs, rs)
    return f

def unavailable():
    """One link, parted — lying flat, where the projection has room for it."""
    f = []
    R, thick, z0, zh, n = 0.62, 0.20, 0.0, 0.26, 40
    for cx, cy, base in ((-0.62, -0.30, math.pi * 0.75), (0.62, 0.30, -math.pi * 0.25)):
        t0, t1 = base + 0.40, base + 2 * math.pi - 0.40
        for i in range(n):
            a0 = t0 + (t1 - t0) * i / n
            a1 = t0 + (t1 - t0) * (i + 1) / n
            ro, ri = R + thick, R - thick
            quad = lambda r: (cx + r * math.cos(a0), cy + r * math.sin(a0),
                              cx + r * math.cos(a1), cy + r * math.sin(a1))
            ox0, oy0, ox1, oy1 = quad(ro)
            ix0, iy0, ix1, iy1 = quad(ri)
            f.append(([p(ox0, oy0, zh), p(ox1, oy1, zh),
                       p(ix1, iy1, zh), p(ix0, iy0, zh)], TOP))          # the top of the link
            mid = (a0 + a1) / 2
            nx, ny = math.cos(mid), math.sin(mid)
            if nx + ny > -0.10:                                           # its outer cheek
                f.append(([p(ox0, oy0, z0), p(ox1, oy1, z0),
                           p(ox1, oy1, zh), p(ox0, oy0, zh)], lit(nx, ny, 0.0)))
            if nx + ny < 0.10:                                            # and the inner one
                f.append(([p(ix0, iy0, z0), p(ix1, iy1, z0),
                           p(ix1, iy1, zh), p(ix0, iy0, zh)], lit(-nx, -ny, 0.0) * 0.7))
    return f

ART = {"activity": activity, "archive": archive, "filtered": filtered,
       "inbox": inbox, "issues": issues, "people": people,
       "projects": projects, "space": space, "specs": specs,
       "unavailable": unavailable}

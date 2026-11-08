#!/usr/bin/env python3
"""Render the website's hero animation (website/src/components/HeroAnimation.tsx) as two looping SVGs for the README.

The canvas animation is simulated once, deterministically, over one loop; every comet, ring and job becomes a SMIL
animation that repeats every LOOP seconds. No scripts, no external refs: GitHub shows it through a plain <img>.

    python3 scripts/readme-hero.py      # writes docs/assets/hero-{light,dark}.svg
"""
import math
import random
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / 'docs' / 'assets'

W, H = 1200, 790
K = 0.8  # the site's scene scale
B = 600  # the scene's base; the title takes the space above the cloud
LOOP = 12.0

# the same scene as the site: label, x as a fraction of the width, height above the cloud's base
CONSUMERS = [
    ('zsh', 0.07, 150), ('bash', 0.14, 40), ('tmux', 0.21, 205), ('nvim', 0.28, 95), ('zsh', 0.355, 0),
    ('fish', 0.38, 175), ('claude', 0.46, 80), ('tmux', 0.53, 215), ('codex', 0.585, 10), ('scanline', 0.63, 135),
    ('nvim', 0.71, 55), ('zsh', 0.775, 190), ('bash', 0.83, 100), ('tmux', 0.91, 30), ('fish', 0.93, 170),
]
SOURCES = [
    ('git', 0.27, 105, 'watch', 1.6), ('battery', 0.385, 88, 'poll', 0.6), ('network', 0.50, 105, 'poll', 1.0),
    ('kube', 0.615, 88, 'watch', 1.2), ('load', 0.73, 105, 'poll', 0.4),
]
READS = {
    'zsh': ['git', 'kube'], 'bash': ['git', 'kube'], 'fish': ['git', 'kube'], 'tmux': ['battery', 'network', 'load'],
    'nvim': ['git'], 'claude': ['git', 'kube'], 'codex': ['git'], 'scanline': ['git', 'kube', 'battery'],
}
CLOUD, COMB_DOWN = 330, 225

# Text colours are the site's, and each clears 7:1 against its chip and the background (checked in main()).
THEMES = {
    'dark': dict(
        bg='#0d1117', chip='#161c26', fg='#e6edf3', title='#e8dfc8',
        chaos=dict(fiber='rgba(255,120,50,0.09)', stroke='rgba(255,150,90,0.55)', text='#ffb38a', label='#ff9a73',
                   dim='rgba(255,150,90,0.75)', out='rgb(255,140,50)', back='rgb(255,205,120)', heat='rgb(255,110,40)'),
        hub=dict(fiber='rgba(70,215,255,0.11)', stroke='rgba(90,225,255,0.6)', text='#7fe9ff', label='#06d6f0',
                 dim='rgba(90,225,255,0.75)', out='rgb(80,220,255)', back='rgb(200,250,255)', heat='rgb(60,210,255)'),
        head='#ffffff', comb_core='#1d3440', seam='rgba(255,255,255,0.35)', handle='rgba(255,255,255,0.8)',
        arrow='#333333'),
    'light': dict(
        bg='#F5F0E8', chip='#fbf8f2', fg='#2c2416', title='#2c2416',
        chaos=dict(fiber='rgba(190,70,20,0.13)', stroke='rgba(170,60,15,0.55)', text='#842405', label='#842405',
                   dim='rgba(132,36,5,0.85)', out='rgb(205,75,15)', back='rgb(150,45,5)', heat='rgb(225,95,30)'),
        hub=dict(fiber='rgba(6,110,140,0.15)', stroke='rgba(6,100,130,0.6)', text='#04505f', label='#04505f',
                 dim='rgba(4,80,95,0.85)', out='rgb(6,125,160)', back='rgb(4,80,95)', heat='rgb(8,145,178)'),
        head=None, comb_core='#ffffff', seam='rgba(0,0,0,0.25)', handle='rgba(44,36,22,0.7)',
        arrow='rgba(255,255,255,0.8)'),
}

FONT = "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif"
MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"


def icons():
    src = (ROOT / 'website/src/components/heroIcons.ts').read_text()
    return dict(re.findall(r"^\s*(\w+): '([^']+)'", src, re.M))


def curve(a, b, bend):
    mx, my = (a[0] + b[0]) / 2, (a[1] + b[1]) / 2
    dx, dy = b[0] - a[0], b[1] - a[1]
    return (a, (mx - dy * bend, my + dx * bend), b)


def d_of(k):
    (ax, ay), (cx, cy), (bx, by) = k
    return f'M{ax:.1f} {ay:.1f}Q{cx:.1f} {cy:.1f} {bx:.1f} {by:.1f}'


def rev(k):
    return (k[2], k[1], k[0])


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


def simulate():
    """The site's schedule(), run once over LOOP seconds. Returns everything that moves, with absolute times."""
    rnd = random.Random(7)
    cons = [dict(label=l, p=(rx * W, B - (CLOUD + up) * K), next=0.4 + i * 0.11) for i, (l, rx, up) in enumerate(CONSUMERS)]
    srcs = [dict(label=l, p=(rx * W, B - down * K), type=ty, cost=c, every=(3.2 + i * 0.6) if ty == 'poll' else (4.5 + i * 0.8),
                 next=1.2 + i * 0.7) for i, (l, rx, down, ty, c) in enumerate(SOURCES)]
    comb = (0.5 * W, B - COMB_DOWN * K)
    links = [(ci, next(i for i, s in enumerate(srcs) if s['label'] == n)) for ci, c in enumerate(cons) for n in READS[c['label']]]
    chaos_k = [curve(cons[ci]['p'], srcs[si]['p'], ((i * 7) % 5 - 2) * 0.07) for i, (ci, si) in enumerate(links)]
    hub_up = [curve(c['p'], comb, (1 if c['p'][0] < comb[0] else -1) * 0.06) for c in cons]
    hub_down = [curve(comb, s['p'], (1 if s['p'][0] < comb[0] else -1) * 0.05) for s in srcs]

    ev = dict(chaos_comets=[], hub_comets=[], chaos_rings=[], hub_rings=[], chaos_jobs=[], hub_jobs=[], flashes=[], waits=[])
    # the counters' events, as on the site: answers given and processes spawned, per side
    count = dict(la=[], lc=[], ra=[], rc=[])
    ev['count'] = count

    def chaos_ask(ci, si, t, shown=True):
        i = links.index((ci, si))
        s, c = srcs[si], cons[ci]
        d = dist(s['p'], c['p']) / 520
        ev['chaos_comets'].append((chaos_k[i], t, d, 'out', 2.6, 'inout'))
        j0 = t + d
        ev['chaos_jobs'].append((si, j0, s['cost']))
        count['lc'].append(j0)
        back = j0 + s['cost']
        ev['chaos_comets'].append((rev(chaos_k[i]), back, d, 'back', 2.4, 'inout'))
        ev['waits'].append((ci, t, back + d))
        ev['flashes'].append(('chaos', ci, back + d))
        if shown:  # a re-run after a change answers nobody who asked
            count['la'].append(back + d)

    def hub_ask(ci, t):
        k = hub_up[ci]
        d = dist(comb, cons[ci]['p'])
        up, down = d / 1700, d / 2200
        ev['hub_comets'].append((k, t, up, 'out', 2.0, 'out'))
        ev['hub_comets'].append((rev(k), t + up, down, 'back', 2.4, 'out'))
        ev['flashes'].append(('hub', ci, t + up + down))
        count['ra'].append(t + up + down)

    busy = {}

    def hub_run(si, t):
        if busy.get(si, -1) > t:
            return
        busy[si] = t + srcs[si]['cost']
        ev['hub_jobs'].append((si, t, srcs[si]['cost']))
        count['rc'].append(t)
        ev['hub_comets'].append((rev(hub_down[si]), t + srcs[si]['cost'], 0.6, 'back', 2.6, 'inout'))

    def tick(si, t):
        s = srcs[si]
        if s['type'] == 'watch':
            mine = [ci for ci, sj in links if sj == si]
            reach = max(dist(cons[ci]['p'], s['p']) for ci in mine)
            ev['chaos_rings'].append((s['p'], t, reach / 300, reach + 30))
            for ci in mine:
                chaos_ask(ci, si, t + dist(cons[ci]['p'], s['p']) / 300, shown=False)
            to = dist(comb, s['p'])
            ev['hub_rings'].append((s['p'], t, to / 320, to + 20))
            t += to / 320
        ev['hub_comets'].append((hub_down[si], t, 0.5, 'out', 2.0, 'inout'))
        hub_run(si, t + 0.5)

    t, dt = 0.0, 1 / 60
    while t < LOOP:
        for ci, c in enumerate(cons):
            if t >= c['next']:
                want = rnd.choice(READS[c['label']])
                si = next(i for i, s in enumerate(srcs) if s['label'] == want)
                chaos_ask(ci, si, t)
                hub_ask(ci, t)
                c['next'] = t + 2.2 + rnd.random() * 1.6
        for si, s in enumerate(srcs):
            if t >= s['next']:
                tick(si, t)
                s['next'] = t + s['every']
        t += dt
    return cons, srcs, comb, links, chaos_k, hub_up, hub_down, ev


def f(x):
    return f'{x:.3f}'.rstrip('0').rstrip('.')


def window(start, dur):
    """Show something from `start` for `dur` seconds, every LOOP seconds."""
    a = min(dur / LOOP, 0.999)
    return f'begin="{f(start)}s" dur="{f(LOOP)}s" repeatCount="indefinite"', a


CURVES = {}  # every curve once, in <defs>; comets point at them


def cid(k):
    return CURVES.setdefault(k, f'k{len(CURVES)}')


def comet(k, start, dur, side, kind, size, ease, T):
    """A request going out or an answer coming back: a dot with a short trail, along one curve."""
    timing, a = window(start, dur)
    pid = cid(k)
    rgb = T[side][kind]
    spline = '0.65 0 0.35 1' if ease == 'inout' else '0.33 1 0.68 1'
    vis = f'<animate attributeName="opacity" values="1;0" keyTimes="0;{f(a)}" calcMode="discrete" {timing}/>'
    ease_at = f'calcMode="spline" keyTimes="0;{f(a)};1" keySplines="{spline};0 0 1 1" {timing}'
    trail = (f'<use href="#{pid}" class="t" stroke="{rgb}" stroke-width="{f(size * 1.1)}">'
             f'<animate attributeName="stroke-dashoffset" values=".18;-.82;-.82" {ease_at}/></use>')
    glow = f'<circle r="{f(size * 5)}" fill="url(#g{side[0]})"/>' if T['head'] else ''
    dot = (f'<g>{glow}<circle r="{f(size * 0.9)}" fill="{T["head"] or rgb}"/>'
           f'<animateMotion keyPoints="0;1;1" {ease_at}><mpath href="#{pid}"/></animateMotion></g>')
    return f'<g opacity="0">{vis}{trail}{dot}</g>'


def ring(p, start, dur, mx, rgb):
    timing, a = window(start, dur)
    return (f'<circle cx="{f(p[0])}" cy="{f(p[1])}" r="12" fill="none" stroke="{rgb}" stroke-width="2" opacity="0">'
            f'<animate attributeName="r" values="12;{f(12 + mx)};{f(12 + mx)}" keyTimes="0;{f(a)};1" {timing}/>'
            f'<animate attributeName="opacity" values="0.6;0;0" keyTimes="0;{f(a)};1" {timing}/></circle>')


def job(p, slot, start, dur, rgb):
    timing, a = window(start, dur)
    r = 15 + slot * 3.5
    return (f'<circle cx="{f(p[0])}" cy="{f(p[1])}" r="{f(r)}" pathLength="1" fill="none" stroke="{rgb}" stroke-width="2" '
            f'stroke-linecap="round" stroke-dasharray="0 1" transform="rotate(-90 {f(p[0])} {f(p[1])})" opacity="0">'
            f'<animate attributeName="opacity" values="1;0" keyTimes="0;{f(a)}" calcMode="discrete" {timing}/>'
            f'<animate attributeName="stroke-dasharray" values="0 1;1 0;1 0" keyTimes="0;{f(a)};1" {timing}/></circle>')


def chip(p, label, side, T, ic, extra=''):
    x, y = p
    tw = len(label) * 8.6  # 600 15px system-ui, near enough for centring
    w, h = max(92, tw + 46), 34
    left = x - (16 + 7 + tw) / 2
    icon = (f'<path d="{ic[label]}" fill="{side["text"]}" transform="translate({f(left)} {f(y - 8)}) scale(0.6667)"/>'
            if label in ic else '')
    return (f'<rect x="{f(x - w / 2)}" y="{f(y - h / 2)}" width="{f(w)}" height="{h}" rx="9" fill="{T["chip"]}" '
            f'stroke="{side["stroke"]}" stroke-width="1.5">{extra}</rect>{icon}'
            f'<text x="{f(left + 23)}" y="{f(y + 1)}" dominant-baseline="middle" fill="{side["text"]}">{label}</text>')


def flash_anim(times, rgb, base):
    """A chip's outline lights when an answer lands, and fades over 0.6 s."""
    out = []
    for t in times:
        timing, a = window(t, 0.6)
        out.append(f'<animate attributeName="stroke" values="{rgb};{base};{base}" keyTimes="0;{f(a)};1" {timing}/>')
    return ''.join(out)


def wait_anim(spans, rgb):
    """While a tool waits on its own run of the command, its outline is dashed and marching."""
    out = []
    for s, e in spans:
        timing, a = window(s, e - s)
        out.append(f'<animate attributeName="stroke-dasharray" values="5 5;none" keyTimes="0;{f(a)}" calcMode="discrete" {timing}/>')
    return ''.join(out)


def merge(spans):
    spans = sorted(spans)
    out = []
    for s, e in spans:
        if out and s <= out[-1][1]:
            out[-1][1] = max(out[-1][1], e)
        else:
            out.append([s, e])
    return out


LEFT_MS, RIGHT_MS = 30.9, 0.015  # what one answer costs, as on the site: running the command, against reading comb


def ticker(times, fmt):
    """A counter that steps up at each of `times` and starts again every loop: one <text> per value, each shown
    only while it is current. SMIL cannot change text, so the values are stacked and swapped."""
    ts = sorted(t % LOOP for t in times)
    edges = [0.0] + ts + [LOOP]
    out = []
    for n in range(len(edges) - 1):
        a, b = edges[n] / LOOP, edges[n + 1] / LOOP
        if b <= a:
            continue
        if a == 0:
            vals, kt = '1;0', f'0;{f(b)}'
        elif b >= 1:
            vals, kt = '0;1', f'0;{f(a)}'
        else:
            vals, kt = '0;1;0', f'0;{f(a)};{f(b)}'
        out.append(f'<text opacity="0">{fmt(n)}<animate attributeName="opacity" values="{vals}" keyTimes="{kt}" '
                   f'calcMode="discrete" dur="{f(LOOP)}s" repeatCount="indefinite"/></text>')
    return ''.join(out)


def stats(T, count):
    """Each side's counters, laid out as on the site: its name, four figures in a row, and the bar of time spent
    waiting, both bars on one scale."""
    out = []
    y = B + 40
    G = 510  # one side's width
    cols = [0, 78, 228, 378]
    for side, x0, left in ((T['chaos'], 40, True), (T['hub'], W - 40 - G, False)):
        k = 'l' if left else 'r'
        ms = LEFT_MS if left else RIGHT_MS
        figs = [
            ('answers', ticker(count[k + 'a'], str)),
            ('processes spawned', ticker(count[k + 'c'], str)),
            ('each answer', f'<text>{ms:.3f} ms</text>'),
            ('spent waiting', ticker(count[k + 'a'], lambda n: f'{n * ms / 1000:.3f} s')),
        ]
        abbr, name = ('BC', 'Before Comb') if left else ('AD', 'After Daemon')
        if left:
            out.append(f'<text x="{x0}" y="{y + 30}" font-size="30" font-weight="800" fill="{side["label"]}">{abbr}'
                       f'<tspan dx="10" font-size="17" font-weight="400" fill="{T["fg"]}">{name}</tspan></text>')
        else:
            out.append(f'<text x="{x0 + G}" y="{y + 30}" text-anchor="end" font-size="17" font-weight="400" fill="{T["fg"]}">'
                       f'{name}<tspan dx="10" font-size="30" font-weight="800" fill="{side["label"]}">{abbr}</tspan></text>')
        for (label, value), cx in zip(figs, cols):
            out.append(f'<g transform="translate({x0 + cx} {y + 72})" font-family="{MONO}" font-size="24" font-weight="700" '
                       f'fill="{side["label"]}">{value}</g>'
                       f'<text x="{x0 + cx}" y="{y + 96}" font-size="15" font-weight="400" fill="{T["fg"]}">{label}</text>')
        # the longer wait fills its bar; comb's is a sliver on the same scale
        track = f'<rect x="{x0}" y="{y + 112}" width="{G}" height="8" rx="4" fill="{T["fg"]}" opacity="0.18"/>'
        bar = (f'<rect x="{x0}" y="{y + 112}" width="{G}" height="8" rx="4" fill="{side["label"]}"/>' if left else
               f'<rect x="{x0 + G - 4}" y="{y + 112}" width="4" height="8" rx="2" fill="{side["label"]}"/>')
        out.append(track + bar)
    return out


def render(theme, sim, ic):
    T = THEMES[theme]
    cons, srcs, comb, links, chaos_k, hub_up, hub_down, ev = sim
    C, Hh = T['chaos'], T['hub']
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="{FONT}" '
         f'font-size="15" font-weight="600" role="img" aria-labelledby="t">'
         f'<title id="t">beachcomber: faster prompts, less load. Without comb, every tool runs the same commands itself; with comb, the daemon runs each once and '
         f'every tool reads the cached answer.</title>',
         '<defs>',
         f'<radialGradient id="gc"><stop offset="0" stop-color="{C["out"]}"/><stop offset=".35" stop-color="{C["out"]}" stop-opacity=".35"/>'
         f'<stop offset="1" stop-color="{C["out"]}" stop-opacity="0"/></radialGradient>',
         f'<radialGradient id="gh"><stop offset="0" stop-color="{Hh["out"]}"/><stop offset=".35" stop-color="{Hh["out"]}" stop-opacity=".35"/>'
         f'<stop offset="1" stop-color="{Hh["out"]}" stop-opacity="0"/></radialGradient>',
         f'<radialGradient id="combfill" cy="0.4"><stop offset="0" stop-color="{T["comb_core"]}"/><stop offset="1" stop-color="{T["chip"]}"/></radialGradient>',
         f'<radialGradient id="combglow"><stop offset="0" stop-color="{Hh["heat"]}" stop-opacity=".35"/><stop offset="1" stop-color="{Hh["heat"]}" stop-opacity="0"/></radialGradient>',
         ]
    # the wiper: sweeps from 0.75 to 0.25 of the width and back, pausing at each end
    xs = '0.75;0.75;0.25;0.25;0.75;0.75'
    kt = '0;0.12;0.45;0.62;0.95;1'
    sweep = ';'.join(f(float(v) * W) for v in xs.split(';'))
    spl = '0 0 1 1;0.65 0 0.35 1;0 0 1 1;0.65 0 0.35 1;0 0 1 1'
    anim = f'dur="{f(LOOP * 2)}s" repeatCount="indefinite" calcMode="spline" keyTimes="{kt}" keySplines="{spl}"'
    o.append(f'<clipPath id="L"><rect x="0" y="0" width="{W * 0.75}" height="{H}"><animate attributeName="width" values="{sweep}" {anim}/></rect></clipPath>')
    o.append(f'<clipPath id="R"><rect x="{W * 0.75}" y="0" width="{W}" height="{H}"><animate attributeName="x" values="{sweep}" {anim}/></rect></clipPath>')
    o.append('<style>.t{fill:none;stroke-dasharray:.18 2;stroke-dashoffset:.18}</style></defs>')
    o.append(f'<rect width="{W}" height="{H}" rx="16" fill="{T["bg"]}"/>')

    def source_layer(side, jobs, rings, isHub):
        g = []
        g += [ring(p, s, d, m, side['heat']) for p, s, d, m in rings]
        # concurrent runs stack outwards, as on the site
        slots = {}
        for si, s, d in sorted(jobs, key=lambda j: j[1]):
            live = [e for e in slots.setdefault(si, []) if e > s]
            slot = len(live)
            slots[si] = live + [s + d]
            if slot < 8:
                g.append(job(srcs[si]['p'], slot, s, d, side['heat']))
        for s in srcs:
            x, y = s['p']
            g.append(f'<circle cx="{f(x)}" cy="{f(y)}" r="15" fill="none" stroke="{side["dim"]}" stroke-width="1.5"/>'
                     f'<circle cx="{f(x)}" cy="{f(y)}" r="6" fill="{side["heat"]}"/>'
                     f'<text x="{f(x)}" y="{f(y + 42)}" text-anchor="middle" fill="{side["text"]}">{s["label"]}</text>'
                     f'<text x="{f(x)}" y="{f(y + 60)}" text-anchor="middle" font-size="13" font-weight="400" fill="{side["text"]}">{s["type"]}</text>')
        return g

    # ---- left: every tool for itself ----
    o.append('<g clip-path="url(#L)">')
    o += [f'<path d="{d_of(k)}" fill="none" stroke="{C["fiber"]}"/>' for k in chaos_k]
    o += source_layer(C, ev['chaos_jobs'], ev['chaos_rings'], False)
    o += [comet(k, s, d, 'chaos', kind, sz, e, T) for k, s, d, kind, sz, e in ev['chaos_comets']]
    for ci, c in enumerate(cons):
        fl = [t for side, i, t in ev['flashes'] if side == 'chaos' and i == ci]
        waits = merge([(s, e) for i, s, e in ev['waits'] if i == ci])
        o.append(chip(c['p'], c['label'], C, T, ic, flash_anim(fl, C['back'], C['stroke']) + wait_anim(waits, C['back'])))
    o.append('</g>')

    # ---- right: one comb ----
    o.append('<g clip-path="url(#R)">')
    o += [f'<path d="{d_of(k)}" fill="none" stroke="{Hh["fiber"]}"/>' for k in hub_up]
    o += [f'<path d="{d_of(k)}" fill="none" stroke="{Hh["fiber"]}" stroke-width="1.3"/>' for k in hub_down]
    o += source_layer(Hh, ev['hub_jobs'], ev['hub_rings'], True)
    o += [comet(k, s, d, 'hub', kind, sz, e, T) for k, s, d, kind, sz, e in ev['hub_comets']]
    cx, cy = comb
    o.append(f'<circle cx="{f(cx)}" cy="{f(cy)}" r="110" fill="url(#combglow)"/>'
             f'<circle cx="{f(cx)}" cy="{f(cy)}" r="36" fill="url(#combfill)" stroke="{Hh["heat"]}" stroke-width="2.5"/>'
             f'<text x="{f(cx)}" y="{f(cy + 1)}" text-anchor="middle" dominant-baseline="middle" font-size="17" font-weight="700" '
             f'fill="{Hh["text"]}">comb</text>')
    for ci, c in enumerate(cons):
        fl = [t for side, i, t in ev['flashes'] if side == 'hub' and i == ci]
        o.append(chip(c['p'], c['label'], Hh, T, ic, flash_anim(fl, Hh['back'], Hh['stroke'])))
    o.append('</g>')

    # the seam and its handle, riding with the wiper
    tx = ';'.join(f'{f(float(v) * W)} 0' for v in xs.split(';'))
    o.append(f'<g><animateTransform attributeName="transform" type="translate" values="{tx}" {anim}/>'
             f'<line x1="0" y1="{H * 0.3:.0f}" x2="0" y2="{H}" stroke="{T["seam"]}" stroke-width="2"/>'
             f'<rect x="-13" y="{f(cy - 24)}" width="26" height="48" rx="13" fill="{T["handle"]}"/>'
             f'<path d="M-3 {f(cy - 6)}l6 -5v10z" fill="{T["arrow"]}"/>'
             f'<path d="M3 {f(cy + 6)}l-6 -5v10z" fill="{T["arrow"]}"/></g>')

    # the title, as on the site: the binary's name in the accent colour
    o.append(f'<text x="{W / 2}" y="80" text-anchor="middle" font-size="64" font-weight="800" letter-spacing="-2.5" '
             f'fill="{T["title"]}">beach<tspan fill="{Hh["label"]}">comb</tspan>er</text>'
             f'<text x="{W / 2}" y="122" text-anchor="middle" font-size="21" font-weight="400" fill="{T["fg"]}">'
             f'Faster prompts, less load.</text>')

    o += stats(T, ev['count'])
    o.append('</svg>')
    defs = ''.join(f'<path id="{i}" d="{d_of(k)}" pathLength="1" fill="none" stroke-linecap="round"/>' for k, i in CURVES.items())
    return ''.join(o).replace('</defs>', defs + '</defs>', 1)


def lum(hex_):
    c = [int(hex_.lstrip('#')[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    c = [v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4 for v in c]
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]


def contrast(a, b):
    la, lb = sorted((lum(a), lum(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def main():
    for name, T in THEMES.items():
        for bg in (T['bg'], T['chip']):
            for fg in (T['fg'], T['title'], T['chaos']['text'], T['hub']['text'], T['chaos']['label'], T['hub']['label']):
                assert contrast(fg, bg) >= 7, f'{name}: {fg} on {bg} is {contrast(fg, bg):.2f}:1'
    sim = simulate()
    ic = icons()
    OUT.mkdir(parents=True, exist_ok=True)
    for name in THEMES:
        CURVES.clear()
        svg = render(name, sim, ic)
        assert '<script' not in svg and 'xlink:href="http' not in svg and 'href="http' not in svg
        (OUT / f'hero-{name}.svg').write_text(svg)
        print(f'hero-{name}.svg: {len(svg) // 1024} KB')


if __name__ == '__main__':
    main()

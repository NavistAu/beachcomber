import {useEffect, useRef} from 'react';
import styles from './HeroAnimation.module.css';
import {ICONS} from './heroIcons';

// The scene is one composition anchored to the bottom of the hero, in pixels: sources at the bottom,
// comb above them, and a loose cloud of tools above comb. On a tall screen there is room above it for the
// title; on a short one the title slides over the top of the cloud and shades it out.
// up: how far above the cloud's base a tool sits.
const CONSUMERS = [
  {label: 'zsh', rx: 0.07, up: 150},
  {label: 'bash', rx: 0.14, up: 40},
  {label: 'tmux', rx: 0.21, up: 205},
  {label: 'nvim', rx: 0.28, up: 95},
  {label: 'zsh', rx: 0.355, up: 0},
  {label: 'fish', rx: 0.38, up: 175},
  {label: 'claude', rx: 0.46, up: 80},
  {label: 'tmux', rx: 0.53, up: 215},
  {label: 'codex', rx: 0.585, up: 10},
  {label: 'scanline', rx: 0.63, up: 135},
  {label: 'nvim', rx: 0.71, up: 55},
  {label: 'zsh', rx: 0.775, up: 190},
  {label: 'bash', rx: 0.83, up: 100},
  {label: 'tmux', rx: 0.91, up: 30},
  {label: 'fish', rx: 0.93, up: 170},
];

// down: how far above the bottom of the hero a source sits
const SOURCES = [
  {label: 'git', rx: 0.27, down: 105, type: 'watch', cost: 1.6},
  {label: 'battery', rx: 0.385, down: 88, type: 'poll', cost: 0.6},
  {label: 'network', rx: 0.50, down: 105, type: 'poll', cost: 1.0},
  {label: 'kube', rx: 0.615, down: 88, type: 'watch', cost: 1.2},
  {label: 'load', rx: 0.73, down: 105, type: 'poll', cost: 0.4},
];

const COMB = {rx: 0.5, down: 225};
// the cloud's base, above the bottom of the composition
const CLOUD = 330;
// the composition's height (top of the cloud to the bottom), and the room the title block takes above it
const SCENE = 600;
// the title block's height, with its margins
const TITLE = 400;
// the cloud's own height: the band its pattern repeats in, upwards, as idle tools that fade out
const BAND = 250;
// the counters' band, when it sits under the sources
const STATS = 200;

// What one answer costs: running the command, against reading it from comb.
const LEFT_MS = 30.9;
const RIGHT_MS = 0.015;

type RGB = [number, number, number];

interface Theme {
  dark: boolean;
  bg: string;
  chipFill: string;
  chaos: {fiber: string; stroke: string; text: string; dim: string; out: RGB; back: RGB; heat: RGB};
  hub: {fiber: string; stroke: string; text: string; dim: string; out: RGB; back: RGB; heat: RGB};
  seam: RGB;
}

const DARK: Theme = {
  dark: true,
  bg: '#0d1117',
  chipFill: '#161c26',
  chaos: {fiber: 'rgba(255,120,50,0.09)', stroke: 'rgba(255,150,90,0.55)', text: '#ffb38a', dim: 'rgba(255,150,90,0.35)',
    out: [255, 140, 50], back: [255, 205, 120], heat: [255, 110, 40]},
  hub: {fiber: 'rgba(70,215,255,0.11)', stroke: 'rgba(90,225,255,0.6)', text: '#7fe9ff', dim: 'rgba(90,225,255,0.35)',
    out: [80, 220, 255], back: [200, 250, 255], heat: [60, 210, 255]},
  seam: [235, 240, 255],
};

const LIGHT: Theme = {
  dark: false,
  bg: '#F5F0E8',
  chipFill: '#fbf8f2',
  chaos: {fiber: 'rgba(190,70,20,0.13)', stroke: 'rgba(170,60,15,0.55)', text: '#842405', dim: 'rgba(170,60,15,0.3)',
    out: [205, 75, 15], back: [150, 45, 5], heat: [225, 95, 30]},
  hub: {fiber: 'rgba(6,110,140,0.15)', stroke: 'rgba(6,100,130,0.6)', text: '#04505f', dim: 'rgba(6,100,130,0.3)',
    out: [6, 125, 160], back: [4, 80, 95], heat: [8, 145, 178]},
  seam: [60, 50, 35],
};

interface P { x: number; y: number }

// A curve between two points, bent sideways by `bend` (a fraction of its length).
interface Curve { a: P; c: P; b: P }

function curve(a: P, b: P, bend: number): Curve {
  const mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2;
  const dx = b.x - a.x, dy = b.y - a.y;
  return {a, b, c: {x: mx - dy * bend, y: my + dx * bend}};
}

function at(k: Curve, t: number): P {
  const u = 1 - t;
  return {
    x: u * u * k.a.x + 2 * u * t * k.c.x + t * t * k.b.x,
    y: u * u * k.a.y + 2 * u * t * k.c.y + t * t * k.b.y,
  };
}

function reverse(k: Curve): Curve { return {a: k.b, c: k.c, b: k.a}; }

const easeOut = (t: number) => 1 - Math.pow(1 - t, 3);
const easeInOut = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

// Something travelling along a curve: a request going out, or an answer coming back.
interface Comet {
  k: Curve; start: number; dur: number; rgb: RGB; size: number;
  ease: (t: number) => number; done?: () => void; fired?: boolean;
}

interface Ring { x: number; y: number; start: number; dur: number; max: number; rgb: RGB }

export default function HeroAnimation(): JSX.Element {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wiperRef = useRef<HTMLDivElement>(null);
  const statRefs = useRef<(HTMLElement | null)[]>([]);
  const barRefs = useRef<(HTMLElement | null)[]>([]);

  useEffect(() => {
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext('2d')!;
    const container = canvas.parentElement!;
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const font = (getComputedStyle(document.documentElement).getPropertyValue('--ifm-font-family-base') || 'system-ui').trim();

    let W = 0, H = 0;
    let split = 0.8, dragging = false, swept = false;
    const t0 = performance.now();

    // Per-node state. Times are in seconds since t0.
    const consumers = CONSUMERS.map((c, i) => ({
      ...c, x: 0, y: 0,
      next: 0.4 + i * 0.11,
      chaos: {pending: 0, flash: -9},
      hub: {flash: -9},
    }));
    const sources = SOURCES.map((s, i) => ({
      ...s, x: 0, y: 0, heat: 0,
      // without comb: every request starts its own run of the command, all at once
      jobs: [] as {li: number; start: number; shown: boolean}[],
      // with comb: only comb runs it
      hubJob: null as null | {start: number},
      every: s.type === 'poll' ? 3.2 + i * 0.6 : 4.5 + i * 0.8,
      next: 1.2 + i * 0.7,
    }));
    const comb = {x: 0, y: 0, flare: 0};
    // answers given and commands run, per side
    const count = {la: 0, lc: 0, ra: 0, rc: 0};

    // What each tool shows: a prompt shows the branch and the cluster, tmux's status bar the battery,
    // network and load, an editor the branch, a coding agent the branch and cluster it works in. Without comb, each tool runs these itself.
    const READS: Record<string, string[]> = {
      zsh: ['git', 'kube'], bash: ['git', 'kube'], fish: ['git', 'kube'],
      tmux: ['battery', 'network', 'load'], nvim: ['git'],
      claude: ['git', 'kube'], codex: ['git'], scanline: ['git', 'kube', 'battery'],
    };
    const links: {ci: number; si: number}[] = [];
    consumers.forEach((c, ci) => {
      READS[c.label].forEach(name => links.push({ci, si: sources.findIndex(s => s.label === name)}));
    });

    let chaosCurves: Curve[] = [];
    let hubUp: Curve[] = [];
    let hubDown: Curve[] = [];
    let chaosComets: Comet[] = [], hubComets: Comet[] = [];
    let chaosRings: Ring[] = [], hubRings: Ring[] = [];

    function layout() {
      // The scene sits on the bottom of the hero, with the counters under it when there is room for them
      // (otherwise in the bottom corners beside the sources). Given room, the scene grows up to 1.8 times its
      // base height, keeping a gap of GAP under the title; whatever is left goes above the cloud, where the
      // title centres and the idle tools fill in. As the hero gets shorter that space closes, the scene shrinks
      // to its base height, and then the title overlaps the top of the cloud.
      const GAP = 120;
      const below = H - TITLE - SCENE - STATS >= 0;
      const B = H - (below ? STATS : 0);
      const k = Math.min(1.8, Math.max(1, (B - TITLE - GAP) / SCENE));
      const stats = container.querySelector<HTMLElement>(`.${styles.statsContainer}`);
      if (stats) {
        stats.dataset.mode = below ? 'below' : 'corners';
        stats.style.top = below ? `${B + 40}px` : '';
      }
      consumers.forEach(c => { c.x = c.rx * W; c.y = B - (CLOUD + c.up) * k; });
      sources.forEach(s => { s.x = s.rx * W; s.y = B - s.down * k; });
      comb.x = COMB.rx * W; comb.y = B - COMB.down * k;
      container.style.setProperty('--comb-y', `${H - comb.y}px`);
      container.style.setProperty('--cloud-y', `${H - (B - CLOUD * k)}px`);
      // the title centres in the space above the cloud, and overlaps the cloud once that space is too small
      cloudTop = B - (CLOUD + BAND) * k - 20;
      container.parentElement?.style.setProperty('--cloud-top', `${Math.max(0, cloudTop)}px`);
      scale = k;
      ghostLayers.clear();
      chaosCurves = links.map(({ci, si}, i) =>
        curve(consumers[ci], sources[si], ((i * 7) % 5 - 2) * 0.07));
      hubUp = consumers.map(c => curve(c, comb, (c.x < comb.x ? 1 : -1) * 0.06));
      hubDown = sources.map(s => curve(comb, s, (s.x < comb.x ? 1 : -1) * 0.05));
    }

    let cloudTop = 0, scale = 1;

    // Idle tools above the cloud: its pattern repeated upwards, fading out towards the top of the hero.
    // Drawn once per size, theme and side.
    const ghostLayers = new Map<string, HTMLCanvasElement>();
    function ghosts(side: Theme['chaos']): HTMLCanvasElement {
      const key = `${T.dark}:${side === T.chaos ? 'l' : 'r'}`;
      let el = ghostLayers.get(key);
      if (!el) {
        el = document.createElement('canvas');
        el.width = canvas.width; el.height = canvas.height;
        const g = el.getContext('2d')!;
        const d = canvas.width / W;
        g.setTransform(d, 0, 0, d, 0, 0);
        const names = ['tmux', 'zsh', 'nvim', 'bash', 'fish', 'claude', 'codex', 'scanline'];
        for (let band = 1; ; band++) {
          const shift = (band % 2 ? 0.035 : -0.02) * band;
          let any = false;
          CONSUMERS.forEach((c, i) => {
            const y = cloudTop + 20 - (BAND * band - (BAND - c.up)) * scale;
            if (y < -20) return;
            any = true;
            const x = (((c.rx + shift) % 1) + 1) % 1 * W;
            const fade = Math.max(0, Math.min(1, y / Math.max(1, cloudTop)));
            g.globalAlpha = 0.55 * fade * fade;
            chip(x, y, names[(i + band * 3) % names.length], side, false, -9, 0, side === T.chaos ? T.chaos.back : T.hub.back, g);
          });
          if (!any) break;
        }
        ghostLayers.set(key, el);
      }
      return el;
    }

    // pre-drawn dot layers, dropped on resize
    const dotLayers = new Map<string, HTMLCanvasElement>();
    const litLayers = new Map<string, HTMLCanvasElement>();

    function resize() {
      const r = container.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      W = r.width; H = r.height;
      canvas.width = W * dpr; canvas.height = H * dpr;
      canvas.style.width = W + 'px'; canvas.style.height = H + 'px';
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      dotLayers.clear(); litLayers.clear();
      layout();
    }

    const near = (x: number) => Math.abs(x - container.getBoundingClientRect().left - split * W) < 36;
    const move = (x: number) => {
      split = Math.max(0.06, Math.min(0.94, (x - container.getBoundingClientRect().left) / W));
      swept = true;
    };
    const onDown = (e: MouseEvent) => { if (near(e.clientX)) dragging = true; };
    const onMove = (e: MouseEvent) => { if (dragging) move(e.clientX); };
    const onUp = () => { dragging = false; };
    const onTouchStart = (e: TouchEvent) => { if (near(e.touches[0].clientX)) { dragging = true; e.preventDefault(); } };
    const onTouchMove = (e: TouchEvent) => { if (dragging) move(e.touches[0].clientX); };

    container.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    container.addEventListener('touchstart', onTouchStart, {passive: false});
    window.addEventListener('touchmove', onTouchMove);
    window.addEventListener('touchend', onUp);
    window.addEventListener('resize', resize);
    resize();

    // ---- what happens ----

    // Without comb: the tool runs the command itself and waits. Each request starts its own process,
    // and the answer goes back only when that process finishes.
    function chaosAsk(ci: number, t: number, si: number, shown = true) {
      const c = consumers[ci];
      const i = links.findIndex(l => l.ci === ci && l.si === si);
      const l = links[i];
      const k = chaosCurves[i];
      const s = sources[l.si];
      c.chaos.pending++;
      chaosComets.push({k, start: t, dur: Math.hypot(s.x - c.x, s.y - c.y) / 520, rgb: T.chaos.out, size: 2.6, ease: easeInOut,
        done: () => { s.jobs.push({li: i, start: now(), shown}); count.lc++; }});
    }

    function chaosWork(t: number) {
      sources.forEach(s => {
        s.jobs = s.jobs.filter(job => {
          if (t < job.start + s.cost) return true;
          const c = consumers[links[job.li].ci];
          chaosComets.push({k: reverse(chaosCurves[job.li]), start: t, dur: Math.hypot(s.x - c.x, s.y - c.y) / 520, rgb: T.chaos.back, size: 2.4, ease: easeInOut,
            done: () => {
              const n = now();
              c.chaos.pending--; c.chaos.flash = n;
              if (job.shown) count.la++;
            }});
          return false;
        });
      });
    }

    // With comb: the tool asks comb, which already has the answer.
    function hubAsk(ci: number, t: number) {
      const c = consumers[ci];
      const k = hubUp[ci];
      const d = Math.hypot(comb.x - c.x, comb.y - c.y);
      const up = d / 1700, down = d / 2200;
      hubComets.push({k, start: t, dur: up, rgb: T.hub.out, size: 2, ease: easeOut,
        done: () => { comb.flare = Math.min(1, comb.flare + 0.35); }});
      hubComets.push({k: reverse(k), start: t + up, dur: down, rgb: T.hub.back, size: 2.4, ease: easeOut,
        done: () => { c.hub.flash = now(); count.ra++; }});
    }

    // comb runs a source once; the new answer goes to comb, and nobody waits on it.
    function hubRun(si: number, t: number) {
      const s = sources[si];
      if (s.hubJob) return;
      s.hubJob = {start: t};
      count.rc++;
    }

    function hubWork(t: number) {
      sources.forEach((s, si) => {
        if (s.hubJob && t >= s.hubJob.start + s.cost) {
          s.hubJob = null;
          hubComets.push({k: reverse(hubDown[si]), start: t, dur: 0.6, rgb: T.hub.back, size: 2.6, ease: easeInOut,
            done: () => { comb.flare = 1; }});
        }
      });
    }

    // A source changes. Without comb, every tool watching it finds out separately and runs it again.
    // With comb, comb alone runs it once. A polled source is run again on comb's timer.
    function sourceTick(si: number, t: number) {
      const s = sources[si];
      if (s.type === 'watch') {
        const reach = Math.max(...links.filter(l => l.si === si).map(l => Math.hypot(consumers[l.ci].x - s.x, consumers[l.ci].y - s.y)));
        chaosRings.push({x: s.x, y: s.y, start: t, dur: reach / 300, max: reach + 30, rgb: T.chaos.heat});
        links.forEach(l => {
          if (l.si !== si) return;
          const c = consumers[l.ci];
          const arrive = t + Math.hypot(c.x - s.x, c.y - s.y) / 300;
          setTimeout(() => chaosAsk(l.ci, now(), si, false), (arrive - t) * 1000);
        });
        const toComb = Math.hypot(comb.x - s.x, comb.y - s.y);
        hubRings.push({x: s.x, y: s.y, start: t, dur: toComb / 320, max: toComb + 20, rgb: T.hub.heat});
        setTimeout(() => {
          hubComets.push({k: hubDown[si], start: now(), dur: 0.5, rgb: T.hub.out, size: 2, ease: easeInOut,
            done: () => hubRun(si, now())});
        }, (toComb / 320) * 1000);
      } else {
        const k = hubDown[si];
        hubComets.push({k, start: t, dur: 0.5, rgb: T.hub.out, size: 2, ease: easeInOut,
          done: () => hubRun(si, now())});
      }
    }

    const now = () => (performance.now() - t0) / 1000;

    function schedule(t: number) {
      if (reduced) return;
      consumers.forEach((c, i) => {
        if (t >= c.next) {
          const mine = READS[c.label];
          const want = mine[Math.floor(Math.random() * mine.length)];
          const si = sources.findIndex(s => s.label === want);
          chaosAsk(i, t, si);
          hubAsk(i, t);
          c.next = t + 2.2 + Math.random() * 1.6;
        }
      });
      sources.forEach((s, i) => {
        if (t >= s.next) { sourceTick(i, t); s.next = t + s.every; }
      });
      chaosWork(t);
      hubWork(t);
    }

    // ---- drawing ----

    let T: Theme = DARK;
    const rgba = (c: RGB, a: number) => `rgba(${c[0]},${c[1]},${c[2]},${a.toFixed(3)})`;

    function stroke(k: Curve, style: string, w: number) {
      ctx.beginPath(); ctx.moveTo(k.a.x, k.a.y); ctx.quadraticCurveTo(k.c.x, k.c.y, k.b.x, k.b.y);
      ctx.strokeStyle = style; ctx.lineWidth = w; ctx.stroke();
    }

    const sprites = new Map<string, HTMLCanvasElement>();
    function sprite(c: RGB): HTMLCanvasElement {
      const key = c.join(',');
      let el = sprites.get(key);
      if (!el) {
        el = document.createElement('canvas');
        el.width = el.height = 128;
        const g2 = el.getContext('2d')!;
        const g = g2.createRadialGradient(64, 64, 0, 64, 64, 64);
        g.addColorStop(0, rgba(c, 1));
        g.addColorStop(0.35, rgba(c, 0.35));
        g.addColorStop(1, rgba(c, 0));
        g2.fillStyle = g; g2.fillRect(0, 0, 128, 128);
        sprites.set(key, el);
      }
      return el;
    }

    function glowDot(x: number, y: number, r: number, c: RGB, a: number) {
      if (a <= 0.003) return;
      ctx.globalAlpha = Math.min(1, a);
      ctx.drawImage(sprite(c), x - r, y - r, r * 2, r * 2);
      ctx.globalAlpha = 1;
    }

    function drawComets(list: Comet[], t: number) {
      ctx.lineCap = 'round';
      for (const m of list) {
        const age = (t - m.start) / m.dur;
        if (age < 0 || age > 1) continue;
        const p = m.ease(age);
        // the trail: short fading segments behind the head
        const N = 8, len = 0.22;
        let prev = at(m.k, Math.max(0, p - len));
        for (let j = 1; j <= N; j++) {
          const q = at(m.k, Math.max(0, p - len + (len * j) / N));
          const f = j / N;
          ctx.beginPath(); ctx.moveTo(prev.x, prev.y); ctx.lineTo(q.x, q.y);
          ctx.strokeStyle = rgba(m.rgb, f * f * (T.dark ? 0.9 : 0.8));
          ctx.lineWidth = m.size * (0.3 + f * 0.9);
          ctx.stroke();
          prev = q;
        }
        const h = at(m.k, p);
        if (T.dark) glowDot(h.x, h.y, m.size * 7, m.rgb, 0.55);
        ctx.fillStyle = T.dark ? rgba([255, 255, 255], 0.95) : rgba(m.rgb, 1);
        ctx.beginPath(); ctx.arc(h.x, h.y, m.size * 0.85, 0, Math.PI * 2); ctx.fill();
      }
    }

    function drawRings(list: Ring[], t: number) {
      for (const r of list) {
        const p = (t - r.start) / r.dur;
        if (p < 0 || p > 1) continue;
        const e = p;
        ctx.beginPath(); ctx.arc(r.x, r.y, 12 + e * r.max, 0, Math.PI * 2);
        ctx.strokeStyle = rgba(r.rgb, 0.6 * (1 - p * p));
        ctx.lineWidth = 2.5 * (1 - p) + 0.75;
        ctx.stroke();
      }
    }

    const icons: Record<string, Path2D> = Object.fromEntries(
      Object.entries(ICONS).map(([k, d]) => [k, new Path2D(d)]));

    const widths = new Map<string, number>();

    function chip(x: number, y: number, text: string, side: Theme['chaos'], waiting: boolean, flash: number, t: number, heatRgb: RGB, g: CanvasRenderingContext2D = ctx) {
      g.font = `600 15px ${font}`;
      let tw = widths.get(text);
      if (tw === undefined) { tw = g.measureText(text).width; widths.set(text, tw); }
      const w = Math.max(92, tw + 46), h = 34, r = 9;
      const f = Math.max(0, 1 - (t - flash) / 0.6);
      if (f > 0 && g === ctx) glowDot(x, y, 70, heatRgb, (T.dark ? 0.35 : 0.22) * f);
      g.beginPath(); g.roundRect(x - w / 2, y - h / 2, w, h, r);
      g.fillStyle = T.chipFill; g.fill();
      g.lineWidth = 1.5;
      if (waiting) {
        g.setLineDash([5, 5]);
        g.lineDashOffset = -t * 40;
        g.strokeStyle = rgba(heatRgb, 0.9);
      } else {
        g.strokeStyle = f > 0 ? rgba(heatRgb, 0.6 + 0.4 * f) : side.stroke;
      }
      g.stroke();
      g.setLineDash([]);
      g.fillStyle = side.text;
      g.textAlign = 'left'; g.textBaseline = 'middle';
      const left = x - (16 + 7 + tw) / 2;
      const icon = icons[text];
      if (icon) {
        const d = g.canvas.width / W, k = d * 16 / 24;
        g.setTransform(k, 0, 0, k, left * d, (y - 8) * d);
        g.fill(icon);
        g.setTransform(d, 0, 0, d, 0, 0);
      }
      g.fillText(text, left + 23, y + 1);
    }

    function source(s: typeof sources[number], side: Theme['chaos'], heat: number, t: number, isHub: boolean) {
      if (heat > 0.05) glowDot(s.x, s.y, 26 + heat * 9, side.heat, Math.min(0.55, heat * 0.11) * (T.dark ? 1 : 0.7));
      // a source running many copies at once shakes: it is being run over and over
      const jx = !isHub && heat > 2.5 ? (Math.random() - 0.5) * (heat - 2.5) * 0.9 : 0;
      const x = s.x + jx, y = s.y;
      ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.arc(x, y, 15, 0, Math.PI * 2);
      ctx.strokeStyle = side.dim; ctx.stroke();
      const jobs = isHub ? (s.hubJob ? [s.hubJob] : []) : s.jobs;
      jobs.slice(0, 8).forEach((job, n) => {
        const p = Math.min(1, (t - job.start) / s.cost);
        ctx.beginPath(); ctx.arc(x, y, 15 + n * 3.5, -Math.PI / 2, -Math.PI / 2 + p * Math.PI * 2);
        ctx.strokeStyle = rgba(side.heat, 1 - n * 0.06); ctx.lineWidth = 2; ctx.lineCap = 'round'; ctx.stroke();
      });
      ctx.beginPath(); ctx.arc(x, y, 6, 0, Math.PI * 2);
      ctx.fillStyle = rgba(side.heat, jobs.length ? 1 : 0.7); ctx.fill();
      ctx.font = `600 15px ${font}`;
      ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
      ctx.fillStyle = side.text;
      ctx.fillText(s.label, s.x, s.y + 46);
      ctx.font = `400 13px ${font}`;
      ctx.fillStyle = side.dim.replace(/[\d.]+\)$/, T.dark ? '0.75)' : '0.85)');
      ctx.fillText(s.type, s.x, s.y + 64);
    }

    function drawComb(t: number) {
      const f = comb.flare;
      glowDot(comb.x, comb.y, 90 + f * 50, T.hub.heat, (T.dark ? 0.16 : 0.12) + f * (T.dark ? 0.3 : 0.18));
      const g = ctx.createRadialGradient(comb.x, comb.y - 8, 2, comb.x, comb.y, 36);
      g.addColorStop(0, T.dark ? '#1d3440' : '#ffffff');
      g.addColorStop(1, T.chipFill);
      ctx.beginPath(); ctx.arc(comb.x, comb.y, 36, 0, Math.PI * 2);
      ctx.fillStyle = g; ctx.fill();
      ctx.lineWidth = 2 + f * 1.5;
      ctx.strokeStyle = rgba(T.hub.heat, 0.75 + 0.25 * f);
      ctx.stroke();
      ctx.font = `700 17px ${font}`;
      ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
      ctx.fillStyle = T.hub.text;
      ctx.fillText('comb', comb.x, comb.y + 1);
    }

    // A faint dot field, lit where the work is: around hot sources on one side, around comb on the other.
    // The dots are drawn once per size and colour; a light stamps them again, brighter, through three
    // nested circles, which approximates a soft falloff.
    function dots(c: RGB): HTMLCanvasElement {
      const key = `${c.join(',')}:${W}x${H}`;
      let el = dotLayers.get(key);
      if (!el) {
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        el = document.createElement('canvas');
        el.width = W * dpr; el.height = H * dpr;
        const g = el.getContext('2d')!;
        g.scale(dpr, dpr);
        g.fillStyle = rgba(c, 1);
        const gap = 26;
        for (let y = gap / 2; y < H; y += gap) for (let x = gap / 2; x < W; x += gap) g.fillRect(x - 0.75, y - 0.75, 1.5, 1.5);
        dotLayers.set(key, el);
      }
      return el;
    }

    function lit(x0: number, y0: number, R: number, c: RGB): HTMLCanvasElement {
      const key = `${c.join(',')}:${x0},${y0},${R}`;
      let el = litLayers.get(key);
      if (!el) {
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const size = R * 3;
        el = document.createElement('canvas');
        el.width = el.height = Math.ceil(size * 2 * dpr);
        const g = el.getContext('2d')!;
        g.scale(dpr, dpr);
        const gap = 26;
        for (let y = gap / 2; y < H; y += gap) {
          if (Math.abs(y - y0) > size) continue;
          for (let x = gap / 2; x < W; x += gap) {
            if (Math.abs(x - x0) > size) continue;
            const a = Math.exp(-((x - x0) ** 2 + (y - y0) ** 2) / (2 * R * R));
            if (a < 0.02) continue;
            g.fillStyle = rgba(c, a);
            g.fillRect(x - x0 + size - 0.75, y - y0 + size - 0.75, 1.5, 1.5);
          }
        }
        litLayers.set(key, el);
      }
      return el;
    }

    function field(lights: {x: number; y: number; r: number; a: number}[], c: RGB) {
      ctx.globalAlpha = T.dark ? 0.05 : 0.07;
      ctx.drawImage(dots(c), 0, 0, W, H);
      for (const l of lights) {
        if (l.a < 0.01) continue;
        const size = l.r * 3;
        ctx.globalAlpha = Math.min(0.55, l.a);
        ctx.drawImage(lit(l.x, l.y, l.r, c), l.x - size, l.y - size, size * 2, size * 2);
      }
      ctx.globalAlpha = 1;
    }

    let last = now();
    let raf = 0;
    let onScreen = true;
    const seen = new IntersectionObserver(([e]) => {
      const was = onScreen;
      onScreen = e.isIntersecting;
      if (onScreen && !was) { last = now(); raf = requestAnimationFrame(frame); }
    });
    seen.observe(container);

    function frame() {
      const t = now();
      // 60 frames a second is plenty: on a 120 Hz screen, skip every other one
      if (t - last < 0.014) { raf = requestAnimationFrame(frame); return; }
      const dt = Math.min(0.05, t - last); last = t;
      T = document.documentElement.getAttribute('data-theme') === 'dark' ? DARK : LIGHT;

      if (!swept && !dragging) {
        const p = (t - 1) / 3;
        if (p > 0) split = p >= 1 ? 0.4 : 0.8 - 0.4 * easeInOut(p);
        if (p >= 1) swept = true;
      }
      const sx = split * W;
      if (wiperRef.current) wiperRef.current.style.left = sx + 'px';

      schedule(t);
      for (const list of [chaosComets, hubComets]) {
        for (const m of list) if (!m.fired && t >= m.start + m.dur) { m.fired = true; m.done?.(); }
      }
      chaosComets = chaosComets.filter(m => t < m.start + m.dur + 0.05);
      hubComets = hubComets.filter(m => t < m.start + m.dur + 0.05);
      chaosRings = chaosRings.filter(r => t < r.start + r.dur);
      hubRings = hubRings.filter(r => t < r.start + r.dur);
      sources.forEach(s => { const want = s.jobs.length; s.heat += (want - s.heat) * Math.min(1, dt * 4); });
      comb.flare *= Math.exp(-dt * 3);

      ctx.fillStyle = T.bg;
      ctx.fillRect(0, 0, W, H);

      // ---- left: every tool for itself ----
      ctx.save();
      ctx.beginPath(); ctx.rect(0, 0, sx, H); ctx.clip();
      field(sources.map(s => ({x: s.x, y: s.y, r: 100, a: s.heat * 0.07})), T.chaos.heat);
      chaosCurves.forEach(k => stroke(k, T.chaos.fiber, 1));
      for (const m of chaosComets) if (t >= m.start && t <= m.start + m.dur) stroke(m.k, rgba(T.chaos.out, T.dark ? 0.22 : 0.3), 1.4);
      drawRings(chaosRings, t);
      if (T.dark) ctx.globalCompositeOperation = 'lighter';
      drawComets(chaosComets, t);
      ctx.globalCompositeOperation = 'source-over';
      sources.forEach(s => source(s, T.chaos, s.heat, t, false));
      ctx.drawImage(ghosts(T.chaos), 0, 0, W, H);
      consumers.forEach(c => chip(c.x, c.y, c.label, T.chaos, c.chaos.pending > 0, c.chaos.flash, t, T.chaos.back));
      ctx.restore();

      // ---- right: one comb ----
      ctx.save();
      ctx.beginPath(); ctx.rect(sx, 0, W - sx, H); ctx.clip();
      field([{x: comb.x, y: comb.y, r: 120, a: 0.12 + comb.flare * 0.25}], T.hub.heat);
      hubUp.forEach(k => stroke(k, T.hub.fiber, 1));
      for (const m of hubComets) if (t >= m.start && t <= m.start + m.dur) stroke(m.k, rgba(T.hub.out, T.dark ? 0.2 : 0.28), 1.4);
      hubDown.forEach(k => stroke(k, T.hub.fiber, 1.3));
      drawRings(hubRings, t);
      if (T.dark) ctx.globalCompositeOperation = 'lighter';
      drawComets(hubComets, t);
      ctx.globalCompositeOperation = 'source-over';
      sources.forEach(s => source(s, T.hub, s.hubJob ? 1 : 0, t, true));
      drawComb(t);
      ctx.drawImage(ghosts(T.hub), 0, 0, W, H);
      consumers.forEach(c => chip(c.x, c.y, c.label, T.hub, false, c.hub.flash, t, T.hub.back));
      ctx.restore();

      // the seam
      const sg = ctx.createLinearGradient(0, 0, 0, H);
      sg.addColorStop(0.3, rgba(T.seam, 0));
      sg.addColorStop(0.6, rgba(T.seam, T.dark ? 0.08 : 0.06));
      sg.addColorStop(1, rgba(T.seam, 0));
      ctx.fillStyle = sg;
      ctx.fillRect(sx - 14, 0, 28, H);

      // time the tools spent waiting for answers, at the real per-answer cost on each side
      const lw = count.la * LEFT_MS / 1000, rw = count.ra * RIGHT_MS / 1000;
      const v = [count.la, count.lc, count.ra, count.rc, `${lw.toFixed(3)} s`, `${rw.toFixed(3)} s`];
      statRefs.current.forEach((el, n) => { if (el && el.textContent !== String(v[n])) el.textContent = String(v[n]); });
      // the bars share one scale: the longer wait fills its bar
      const most = Math.max(lw, rw, 1e-9);
      barRefs.current.forEach((el, n) => { if (el) el.style.width = `max(2px, ${(100 * [lw, rw][n] / most).toFixed(3)}%)`; });

      if (onScreen) raf = requestAnimationFrame(frame);
    }
    raf = requestAnimationFrame(frame);

    return () => {
      cancelAnimationFrame(raf);
      seen.disconnect();
      window.removeEventListener('resize', resize);
      container.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      container.removeEventListener('touchstart', onTouchStart);
      window.removeEventListener('touchmove', onTouchMove);
      window.removeEventListener('touchend', onUp);
    };
  }, []);

  return (
    <div className={styles.animationContainer}>
      <canvas ref={canvasRef} />
      <div className={styles.topFade} />
      <div className={styles.bottomFade} />
      <div className={styles.wiper} ref={wiperRef}>
        <div className={styles.wiperHandle}>
          <div className={`${styles.wiperArrow} ${styles.left}`} />
          <div className={`${styles.wiperArrow} ${styles.right}`} />
        </div>
      </div>
      <div className={styles.statsContainer}>
        <div className={`${styles.statsGroup} ${styles.chaos}`}>
          <h2 className={styles.era}><abbr title="Before Comb">BC</abbr><span>Before Comb</span></h2>
          <dl>
            <div className={styles.statItem}><dt>answers</dt><dd ref={el => { statRefs.current[0] = el; }}>0</dd></div>
            <div className={styles.statItem}><dt>processes spawned</dt><dd ref={el => { statRefs.current[1] = el; }}>0</dd></div>
            <div className={styles.statItem}><dt>each answer</dt><dd>30.900 ms</dd></div>
            <div className={styles.statItem}><dt>spent waiting</dt><dd ref={el => { statRefs.current[4] = el; }}>0.000 s</dd></div>
          </dl>
          <div className={styles.bar} aria-hidden="true"><span ref={el => { barRefs.current[0] = el; }} /></div>
        </div>
        <div className={`${styles.statsGroup} ${styles.hub}`}>
          <h2 className={styles.era}><abbr title="After Daemon">AD</abbr><span>After Daemon</span></h2>
          <dl>
            <div className={styles.statItem}><dt>answers</dt><dd ref={el => { statRefs.current[2] = el; }}>0</dd></div>
            <div className={styles.statItem}><dt>processes spawned</dt><dd ref={el => { statRefs.current[3] = el; }}>0</dd></div>
            <div className={styles.statItem}><dt>each answer</dt><dd>0.015 ms</dd></div>
            <div className={styles.statItem}><dt>spent waiting</dt><dd ref={el => { statRefs.current[5] = el; }}>0.000 s</dd></div>
          </dl>
          <div className={styles.bar} aria-hidden="true"><span ref={el => { barRefs.current[1] = el; }} /></div>
        </div>
      </div>
    </div>
  );
}

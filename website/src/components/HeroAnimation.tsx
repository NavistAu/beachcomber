import {useEffect, useRef, useCallback} from 'react';
import styles from './HeroAnimation.module.css';

// Layout (relative coords 0-1)
const consumerRel = [
  {label: 'zsh', rx: 0.08, ry: 0.06},
  {label: 'bash', rx: 0.24, ry: 0.02},
  {label: 'tmux', rx: 0.40, ry: 0.08},
  {label: 'nvim', rx: 0.56, ry: 0.02},
  {label: 'fish', rx: 0.72, ry: 0.06},
  {label: 'tmux', rx: 0.88, ry: 0.03},
  {label: 'zsh', rx: 0.16, ry: 0.20},
  {label: 'tmux', rx: 0.34, ry: 0.22},
  {label: 'nvim', rx: 0.50, ry: 0.18},
  {label: 'bash', rx: 0.66, ry: 0.21},
  {label: 'zsh', rx: 0.82, ry: 0.18},
];

const sourceRel = [
  {label: 'git', rx: 0.10, ry: 0.85, type: 'watch'},
  {label: 'battery', rx: 0.30, ry: 0.88, type: 'poll'},
  {label: 'network', rx: 0.50, ry: 0.85, type: 'poll'},
  {label: 'kube', rx: 0.70, ry: 0.88, type: 'watch'},
  {label: 'load', rx: 0.90, ry: 0.85, type: 'poll'},
];

const DAEMON_REL = {rx: 0.50, ry: 0.54};

interface Packet {
  x1: number; y1: number; x2: number; y2: number;
  start: number; dur: number; color: string; size: number;
  onArrive?: () => void; arrived?: boolean;
}

interface Ripple {
  x: number; y: number; start: number; max: number; spd: number;
}

interface Consumer {
  label: string; rx: number; ry: number; x: number; y: number;
  chaosState: string; chaosUntil: number;
  hubState: string; hubUntil: number;
}

interface Source {
  label: string; rx: number; ry: number; x: number; y: number; type: string;
}

export default function HeroAnimation(): JSX.Element {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wiperRef = useRef<HTMLDivElement>(null);
  const stateRef = useRef({
    splitX: 0.8,
    targetSplitX: 0.8,
    dragging: false,
    autoSweepDone: false,
    autoSweepStart: 0,
    consumers: [] as Consumer[],
    sources: [] as Source[],
    daemon: {x: 0, y: 0},
    chaosConns: [] as {ci: number; si: number}[],
    cPkt: [] as Packet[],
    hPkt: [] as Packet[],
    cRip: [] as Ripple[],
    hRip: [] as Ripple[],
    T0: 0,
    W: 0,
    H: 0,
  });

  const init = useCallback(() => {
    const s = stateRef.current;
    s.consumers = consumerRel.map(c => ({
      ...c, x: 0, y: 0,
      chaosState: 'idle', chaosUntil: 0,
      hubState: 'idle', hubUntil: 0,
    }));
    s.sources = sourceRel.map(sr => ({...sr, x: 0, y: 0}));
    s.daemon = {x: 0, y: 0};
    s.chaosConns = [];
    consumerRel.forEach((_, ci) => {
      const count = 2 + Math.floor(Math.random() * 2);
      const picked = new Set<number>();
      for (let j = 0; j < count; j++) {
        let si: number;
        do { si = Math.floor(Math.random() * sourceRel.length); } while (picked.has(si));
        picked.add(si);
        s.chaosConns.push({ci, si});
      }
    });
    s.cPkt = []; s.hPkt = []; s.cRip = []; s.hRip = [];
    s.T0 = performance.now();
    s.autoSweepStart = s.T0 + 1000; // start sweep after 1s
    s.splitX = 0.8;
    s.autoSweepDone = false;
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    const s = stateRef.current;

    function resize() {
      const rect = canvas!.parentElement!.getBoundingClientRect();
      s.W = rect.width;
      s.H = rect.height;
      canvas!.width = s.W * 2;
      canvas!.height = s.H * 2;
      canvas!.style.width = s.W + 'px';
      canvas!.style.height = s.H + 'px';
      ctx!.setTransform(2, 0, 0, 2, 0, 0);
    }

    window.addEventListener('resize', resize);
    resize();
    init();

    // Drag handlers
    const container = canvas!.parentElement!;

    const onMouseDown = (e: MouseEvent) => {
      // Only start drag if near the wiper
      const wiperX = s.splitX * s.W;
      if (Math.abs(e.clientX - container.getBoundingClientRect().left - wiperX) < 30) {
        s.dragging = true;
      }
    };
    const onMouseMove = (e: MouseEvent) => {
      if (!s.dragging) return;
      const rect = container.getBoundingClientRect();
      s.splitX = Math.max(0.08, Math.min(0.92, (e.clientX - rect.left) / s.W));
      s.autoSweepDone = true;
    };
    const onMouseUp = () => { s.dragging = false; };
    const onTouchStart = (e: TouchEvent) => {
      const wiperX = s.splitX * s.W;
      const rect = container.getBoundingClientRect();
      if (Math.abs(e.touches[0].clientX - rect.left - wiperX) < 40) {
        s.dragging = true;
        e.preventDefault();
      }
    };
    const onTouchMove = (e: TouchEvent) => {
      if (!s.dragging) return;
      const rect = container.getBoundingClientRect();
      s.splitX = Math.max(0.08, Math.min(0.92, (e.touches[0].clientX - rect.left) / s.W));
      s.autoSweepDone = true;
    };
    const onTouchEnd = () => { s.dragging = false; };

    container.addEventListener('mousedown', onMouseDown);
    window.addEventListener('mousemove', onMouseMove);
    window.addEventListener('mouseup', onMouseUp);
    container.addEventListener('touchstart', onTouchStart, {passive: false});
    window.addEventListener('touchmove', onTouchMove);
    window.addEventListener('touchend', onTouchEnd);

    // Drawing helpers
    function roundRect(x: number, y: number, w: number, h: number, r: number, fill: string | null, stroke: string | null, lw?: number) {
      ctx!.beginPath();
      ctx!.moveTo(x-w/2+r,y-h/2); ctx!.lineTo(x+w/2-r,y-h/2);
      ctx!.arcTo(x+w/2,y-h/2,x+w/2,y-h/2+r,r);
      ctx!.lineTo(x+w/2,y+h/2-r); ctx!.arcTo(x+w/2,y+h/2,x+w/2-r,y+h/2,r);
      ctx!.lineTo(x-w/2+r,y+h/2); ctx!.arcTo(x-w/2,y+h/2,x-w/2,y+h/2-r,r);
      ctx!.lineTo(x-w/2,y-h/2+r); ctx!.arcTo(x-w/2,y-h/2,x-w/2+r,y-h/2,r);
      ctx!.closePath();
      if(fill){ctx!.fillStyle=fill;ctx!.fill();}
      if(stroke){ctx!.strokeStyle=stroke;ctx!.lineWidth=lw||2;ctx!.stroke();}
    }

    function diamondShape(x: number, y: number, sz: number, fill: string | null, stroke: string | null, lw?: number) {
      ctx!.beginPath();
      ctx!.moveTo(x,y-sz); ctx!.lineTo(x+sz*1.1,y); ctx!.lineTo(x,y+sz); ctx!.lineTo(x-sz*1.1,y);
      ctx!.closePath();
      if(fill){ctx!.fillStyle=fill;ctx!.fill();}
      if(stroke){ctx!.strokeStyle=stroke;ctx!.lineWidth=lw||2;ctx!.stroke();}
    }

    function circle(x: number, y: number, r: number, fill: string | null, stroke: string | null, lw?: number) {
      ctx!.beginPath(); ctx!.arc(x,y,r,0,Math.PI*2);
      if(fill){ctx!.fillStyle=fill;ctx!.fill();}
      if(stroke){ctx!.strokeStyle=stroke;ctx!.lineWidth=lw||2;ctx!.stroke();}
    }

    function label(x: number, y: number, str: string, color: string, size: number, bold?: boolean) {
      ctx!.fillStyle=color;
      ctx!.font=`${bold?'bold ':''}${size}px system-ui`;
      ctx!.textAlign='center'; ctx!.textBaseline='middle';
      ctx!.fillText(str,x,y);
    }

    function line(x1: number, y1: number, x2: number, y2: number, color: string, w?: number) {
      ctx!.beginPath(); ctx!.moveTo(x1,y1); ctx!.lineTo(x2,y2);
      ctx!.strokeStyle=color; ctx!.lineWidth=w||1; ctx!.stroke();
    }

    // Colors
    const C_IDLE = 'rgb(220,120,50)';
    const C_DIM = 'rgb(120,65,30)';
    const H_IDLE = 'rgb(6,180,210)';
    const H_DIM = 'rgb(6,90,110)';

    function consumerColor(state: string, until: number, now: number, isHub: boolean): string {
      const idle = isHub ? H_IDLE : C_IDLE;
      const dim = isHub ? H_DIM : C_DIM;
      const flashR = isHub ? [120,255,255] : [255,230,70];
      const idleR = isHub ? [6,180,210] : [220,120,50];
      const flashDur = isHub ? 250 : 300;

      if (state === 'waiting' && now < until) return dim;
      if (state === 'flash' && now < until) {
        const mix = Math.max(0, Math.min(1, (until - now) / flashDur));
        return `rgb(${Math.round(idleR[0]+(flashR[0]-idleR[0])*mix)},${Math.round(idleR[1]+(flashR[1]-idleR[1])*mix)},${Math.round(idleR[2]+(flashR[2]-idleR[2])*mix)})`;
      }
      return idle;
    }

    function consumerGlow(state: string, until: number, now: number, isHub: boolean): string | null {
      const flashDur = isHub ? 250 : 300;
      if (state === 'flash' && now < until) {
        const mix = Math.max(0, Math.min(1, (until - now) / flashDur));
        if (mix > 0.5) {
          const c = isHub ? '120,255,255' : '255,230,70';
          return `rgba(${c},${((mix-0.5)*0.4).toFixed(3)})`;
        }
      }
      return null;
    }

    function drawPkts(list: Packet[], t: number) {
      for (const p of list) {
        const age = t - p.start;
        if (age < 0 || age > p.dur) continue;
        const prog = age / p.dur;
        const px = p.x1 + (p.x2 - p.x1) * prog;
        const py = p.y1 + (p.y2 - p.y1) * prog;
        const tp = Math.max(0, prog - 0.2);
        const tx = p.x1 + (p.x2 - p.x1) * tp;
        const ty = p.y1 + (p.y2 - p.y1) * tp;
        const grad = ctx!.createLinearGradient(tx, ty, px, py);
        grad.addColorStop(0, 'rgba(0,0,0,0)');
        grad.addColorStop(1, p.color);
        ctx!.beginPath(); ctx!.moveTo(tx, ty); ctx!.lineTo(px, py);
        ctx!.strokeStyle = grad; ctx!.lineWidth = p.size || 3.5; ctx!.lineCap = 'round'; ctx!.stroke();
        circle(px, py, (p.size || 3.5) * 0.7, p.color, null);
      }
    }

    function drawRips(list: Ripple[], t: number, r: number, g: number, b: number) {
      for (const rp of list) {
        const age = t - rp.start;
        if (age < 0) continue;
        const rad = age * rp.spd;
        if (rad > rp.max) continue;
        const prog = rad / rp.max;
        const op = 0.5 * (1 - prog);
        circle(rp.x, rp.y, rad, null, `rgba(${r},${g},${b},${op.toFixed(3)})`, 2.5);
      }
    }

    function spawnChaos(t: number, now: number) {
      const {consumers: cs, sources: ss, chaosConns: cc, cPkt, cRip} = s;
      cs.forEach((c, ci) => {
        const my = cc.filter(cn => cn.ci === ci);
        if (!my.length) return;
        const iv = 1.0 + (ci % 4) * 0.2;
        if (((t + ci * 0.25) % iv) < 0.017) {
          const cn = my[Math.floor(Math.random() * my.length)];
          const src = ss[cn.si];
          const d = Math.hypot(src.x - c.x, src.y - c.y);
          const downDur = d / 500;
          const upDur = downDur * 1.2;
          c.chaosState = 'waiting';
          c.chaosUntil = now + (downDur + 0.08 + upDur) * 1000;
          cPkt.push({x1: c.x, y1: c.y, x2: src.x, y2: src.y, start: t, dur: downDur, color: 'rgba(255,140,40,0.85)', size: 4});
          cPkt.push({x1: src.x, y1: src.y, x2: c.x, y2: c.y, start: t + downDur + 0.08, dur: upDur, color: 'rgba(232,110,50,0.55)', size: 3, onArrive: () => { c.chaosState = 'flash'; c.chaosUntil = performance.now() + 300; }});
        }
      });
      ss.forEach((src, si) => {
        if (src.type !== 'watch') return;
        const iv = 2.2 + si * 0.8;
        if (((t + si * 0.9) % iv) < 0.017) {
          const connected = cc.filter(cn => cn.si === si).map(cn => cs[cn.ci]);
          let maxD = 0;
          connected.forEach(c => { const d = Math.hypot(c.x - src.x, c.y - src.y); if (d > maxD) maxD = d; });
          cRip.push({x: src.x, y: src.y, start: t, max: maxD + 50, spd: 280});
          connected.forEach((c, idx) => {
            const d = Math.hypot(c.x - src.x, c.y - src.y);
            const arr = d / 280;
            setTimeout(() => { c.chaosState = 'flash'; c.chaosUntil = performance.now() + 300; }, arr * 1000);
            cPkt.push({x1: src.x, y1: src.y, x2: c.x, y2: c.y, start: t + 0.05 + idx * 0.04, dur: arr, color: 'rgba(255,200,30,0.9)', size: 5});
          });
        }
      });
      ss.forEach((src, si) => {
        if (src.type !== 'poll') return;
        const iv = 3.5 + si * 0.7;
        if (((t + si * 1.3) % iv) < 0.017) {
          cRip.push({x: src.x, y: src.y, start: t, max: 50, spd: 160});
        }
      });
    }

    function spawnHub(t: number, now: number) {
      const {consumers: cs, sources: ss, daemon: dm, hPkt, hRip} = s;
      cs.forEach((c, ci) => {
        const iv = 1.4 + (ci % 5) * 0.35;
        if (((t + ci * 0.3) % iv) < 0.017) {
          const d = Math.hypot(dm.x - c.x, dm.y - c.y);
          const downDur = d / 2000;
          const upDur = downDur * 0.5;
          c.hubState = 'waiting';
          c.hubUntil = now + (downDur + 0.02 + upDur) * 1000;
          hPkt.push({x1: c.x, y1: c.y, x2: dm.x, y2: dm.y, start: t, dur: downDur, color: 'rgba(6,220,250,0.7)', size: 4});
          hPkt.push({x1: dm.x, y1: dm.y, x2: c.x, y2: c.y, start: t + downDur + 0.02, dur: upDur, color: 'rgba(6,245,255,0.8)', size: 4.5, onArrive: () => { c.hubState = 'flash'; c.hubUntil = performance.now() + 250; }});
        }
      });
      ss.forEach((src, si) => {
        if (src.type !== 'watch') return;
        const iv = 2.2 + si * 0.8;
        if (((t + si * 0.9 + 0.5) % iv) < 0.017) {
          const dD = Math.hypot(dm.x - src.x, dm.y - src.y);
          hRip.push({x: src.x, y: src.y, start: t, max: dD + 25, spd: 320});
          hPkt.push({x1: src.x, y1: src.y, x2: dm.x, y2: dm.y, start: t + 0.05, dur: dD / 320, color: 'rgba(6,245,255,0.85)', size: 5});
        }
      });
      ss.forEach((src, si) => {
        if (src.type !== 'poll') return;
        const iv = 4.5 + si * 1.0;
        if (((t + si * 1.4) % iv) < 0.017) {
          hRip.push({x: src.x, y: src.y, start: t, max: 45, spd: 160});
          const d = Math.hypot(src.x - dm.x, src.y - dm.y);
          const dur = d / 500;
          hPkt.push({x1: dm.x, y1: dm.y, x2: src.x, y2: src.y, start: t, dur, color: 'rgba(6,200,230,0.5)', size: 3});
          hPkt.push({x1: src.x, y1: src.y, x2: dm.x, y2: dm.y, start: t + dur + 0.05, dur: dur * 0.8, color: 'rgba(6,220,240,0.55)', size: 3});
        }
      });
    }

    let animId: number;

    function tick(now: number) {
      const t = (now - s.T0) / 1000;
      const {W, H} = s;

      // Update positions
      s.consumers.forEach((c, i) => { c.x = consumerRel[i].rx * W; c.y = consumerRel[i].ry * H; });
      s.sources.forEach((src, i) => { src.x = sourceRel[i].rx * W; src.y = sourceRel[i].ry * H; });
      s.daemon.x = DAEMON_REL.rx * W;
      s.daemon.y = DAEMON_REL.ry * H;

      // Auto-sweep: from 0.8 to 0.4 over 3s, starting after 1s
      if (!s.autoSweepDone && !s.dragging) {
        const sweepElapsed = (now - s.autoSweepStart) / 1000;
        if (sweepElapsed > 0 && sweepElapsed < 3) {
          const p = sweepElapsed / 3;
          const eased = p * p * (3 - 2 * p); // smoothstep
          s.splitX = 0.8 - 0.4 * eased;
        } else if (sweepElapsed >= 3) {
          s.splitX = 0.4;
          s.autoSweepDone = true;
        }
      }

      const sx = s.splitX * W;
      ctx!.clearRect(0, 0, W, H);

      spawnChaos(t, now);
      spawnHub(t, now);

      // Check arrivals
      for (const p of s.cPkt) { if (!p.arrived && t - p.start >= p.dur && p.onArrive) { p.onArrive(); p.arrived = true; } }
      for (const p of s.hPkt) { if (!p.arrived && t - p.start >= p.dur && p.onArrive) { p.onArrive(); p.arrived = true; } }

      s.cPkt = s.cPkt.filter(p => t - p.start < p.dur + 0.1);
      s.hPkt = s.hPkt.filter(p => t - p.start < p.dur + 0.1);
      s.cRip = s.cRip.filter(r => (t - r.start) * r.spd < r.max + 5);
      s.hRip = s.hRip.filter(r => (t - r.start) * r.spd < r.max + 5);

      // Update wiper position
      if (wiperRef.current) {
        wiperRef.current.style.left = sx + 'px';
      }

      // ===== LEFT: CHAOS =====
      ctx!.save();
      ctx!.beginPath(); ctx!.rect(0, 0, sx, H); ctx!.clip();

      s.chaosConns.forEach(cn => {
        const c = s.consumers[cn.ci], src = s.sources[cn.si];
        line(c.x, c.y, src.x, src.y, 'rgba(232,68,26,0.07)', 0.6);
      });

      drawRips(s.cRip, t, 255, 180, 30);
      drawPkts(s.cPkt, t);

      s.sources.forEach(src => {
        const tc = src.type === 'watch' ? 'rgba(255,180,30,0.6)' : 'rgba(200,150,80,0.5)';
        diamondShape(src.x, src.y, 30, '#141b24', 'rgb(220,120,50)', 2);
        label(src.x, src.y, src.label, 'rgb(220,120,50)', 13, true);
        label(src.x, src.y + 44, src.type, tc, 11);
      });

      s.consumers.forEach((c) => {
        const col = consumerColor(c.chaosState, c.chaosUntil, now, false);
        const glow = consumerGlow(c.chaosState, c.chaosUntil, now, false);
        if (glow) roundRect(c.x, c.y, 82, 46, 11, null, glow, 1);
        roundRect(c.x, c.y, 72, 36, 8, '#141b24', col, 2);
        label(c.x, c.y, c.label, col, 14, true);
      });

      ctx!.restore();

      // ===== RIGHT: HUB =====
      ctx!.save();
      ctx!.beginPath(); ctx!.rect(sx, 0, W - sx, H); ctx!.clip();

      s.consumers.forEach(c => line(c.x, c.y, s.daemon.x, s.daemon.y, 'rgba(6,214,240,0.055)', 0.6));
      s.sources.forEach(src => line(s.daemon.x, s.daemon.y, src.x, src.y, 'rgba(6,214,240,0.08)', 0.8));

      drawRips(s.hRip, t, 6, 214, 240);
      drawPkts(s.hPkt, t);

      s.sources.forEach(src => {
        const tc = src.type === 'watch' ? 'rgba(6,214,240,0.45)' : 'rgba(6,180,200,0.35)';
        diamondShape(src.x, src.y, 30, '#141b24', 'rgba(6,210,230,0.6)', 2);
        label(src.x, src.y, src.label, 'rgba(6,210,230,0.6)', 13, true);
        label(src.x, src.y + 44, src.type, tc, 11);
      });

      s.consumers.forEach(c => {
        const col = consumerColor(c.hubState, c.hubUntil, now, true);
        const glow = consumerGlow(c.hubState, c.hubUntil, now, true);
        if (glow) roundRect(c.x, c.y, 82, 46, 11, null, glow, 1);
        roundRect(c.x, c.y, 72, 36, 8, '#141b24', col, 2);
        label(c.x, c.y, c.label, col, 14, true);
      });

      // Daemon
      const hit = s.hPkt.some(p => {
        const age = t - p.start;
        return age >= p.dur * 0.85 && age <= p.dur + 0.1 && Math.abs(p.x2 - s.daemon.x) < 15 && Math.abs(p.y2 - s.daemon.y) < 15;
      });
      const gr = hit ? 55 : 45;
      const go = hit ? 0.1 : 0.04;
      circle(s.daemon.x, s.daemon.y, gr, `rgba(6,214,240,${go})`, null);
      circle(s.daemon.x, s.daemon.y, 34, '#0d1117', hit ? 'rgb(80,245,255)' : 'rgb(6,214,240)', 2.5);
      label(s.daemon.x, s.daemon.y - 2, 'comb', hit ? '#80f5ff' : '#06d6f0', 16, true);

      const pp = (t % 3.5);
      if (pp < 0.6) {
        circle(s.daemon.x, s.daemon.y, 34 + pp * 30, null, `rgba(6,214,240,${(0.05 * (1 - pp / 0.6)).toFixed(3)})`, 1.5);
      }

      ctx!.restore();

      animId = requestAnimationFrame(tick);
    }

    animId = requestAnimationFrame(tick);

    return () => {
      cancelAnimationFrame(animId);
      window.removeEventListener('resize', resize);
      container.removeEventListener('mousedown', onMouseDown);
      window.removeEventListener('mousemove', onMouseMove);
      window.removeEventListener('mouseup', onMouseUp);
      container.removeEventListener('touchstart', onTouchStart);
      window.removeEventListener('touchmove', onTouchMove);
      window.removeEventListener('touchend', onTouchEnd);
    };
  }, [init]);

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
      <div className={styles.zoneLabels}>
        <span className={styles.chaosLabel}>Without beachcomber</span>
        <span className={styles.hubLabel}>With beachcomber</span>
      </div>
      <div className={styles.statsContainer}>
        <div className={`${styles.statsGroup} ${styles.chaos}`}>
          <div className={styles.statItem}><div className={styles.statVal}>960</div><div className={styles.statLbl}>threads</div></div>
          <div className={styles.statItem}><div className={styles.statVal}>30</div><div className={styles.statLbl}>watchers</div></div>
          <div className={styles.statItem}><div className={styles.statVal}>312</div><div className={styles.statLbl}>handles</div></div>
          <div className={styles.statItem}><div className={styles.statVal}>30.9ms</div><div className={styles.statLbl}>latency</div></div>
        </div>
        <div className={`${styles.statsGroup} ${styles.hub}`}>
          <div className={styles.statItem}><div className={styles.statVal}>4</div><div className={styles.statLbl}>threads</div></div>
          <div className={styles.statItem}><div className={styles.statVal}>1</div><div className={styles.statLbl}>watcher</div></div>
          <div className={styles.statItem}><div className={styles.statVal}>12</div><div className={styles.statLbl}>handles</div></div>
          <div className={styles.statItem}><div className={styles.statVal}>15&#181;s</div><div className={styles.statLbl}>latency</div></div>
        </div>
      </div>
    </div>
  );
}

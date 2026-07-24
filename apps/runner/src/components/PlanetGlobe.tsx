import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";
import { type GeoSphere, geoGraticule10, geoOrthographic, geoPath } from "d3-geo";
import { useEffect, useRef } from "react";
import { feature } from "topojson-client";
import land110m from "world-atlas/land-110m.json";
import { usePrefersReducedMotion } from "@/hooks/usePrefersReducedMotion";

const LAND = feature(land110m, land110m.objects.land);
const GRATICULE = geoGraticule10();
const SPHERE: GeoSphere = { type: "Sphere" };
/** Southern-hemisphere tilt, purely aesthetic. */
const TILT = -18;
/** ~6°/s → one revolution per minute. */
const DEG_PER_MS = 0.006;

export function PlanetGlobe() {
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // Rotation lives in a ref so remounts (StrictMode) don't snap the globe back.
  const rotationRef = useRef(0);
  const reduced = usePrefersReducedMotion();

  useEffect(() => {
    const wrap = wrapRef.current;
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!wrap || !canvas || !ctx) return;

    const projection = geoOrthographic();
    const path = geoPath(projection, ctx);
    let size = 0;

    const css = (name: string) =>
      getComputedStyle(document.documentElement).getPropertyValue(name).trim();

    const draw = () => {
      if (size === 0) return;
      projection.rotate([rotationRef.current, TILT]);
      ctx.clearRect(0, 0, size, size);
      ctx.lineWidth = 1;
      ctx.beginPath();
      path(SPHERE);
      ctx.strokeStyle = css("--baseline");
      ctx.stroke();
      ctx.globalAlpha = 0.7;
      ctx.beginPath();
      path(GRATICULE);
      ctx.strokeStyle = css("--grid");
      ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.beginPath();
      path(LAND);
      ctx.fillStyle = css("--grid");
      ctx.fill();
      ctx.lineWidth = 0.75;
      ctx.beginPath();
      path(LAND);
      ctx.strokeStyle = css("--ink-secondary");
      ctx.stroke();
    };

    const ro = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (!rect || rect.width === 0) return;
      size = rect.width;
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(size * dpr);
      canvas.height = Math.round(size * dpr);
      canvas.style.width = `${size}px`;
      canvas.style.height = `${size}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      projection.scale(size / 2 - 8).translate([size / 2, size / 2]);
      draw();
    });
    ro.observe(wrap);

    // Theme flips recolor the canvas; this is the only redraw signal when the
    // rotation loop is off for prefers-reduced-motion.
    const mo = new MutationObserver(draw);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });

    let raf = 0;
    if (!reduced) {
      let last = performance.now();
      const tick = (now: number) => {
        rotationRef.current = (rotationRef.current + (now - last) * DEG_PER_MS) % 360;
        last = now;
        draw();
        raf = requestAnimationFrame(tick);
      };
      raf = requestAnimationFrame(tick);
    }

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      mo.disconnect();
    };
  }, [reduced]);

  return (
    <Card>
      <CardHeader>
        <CardTitle>planet</CardTitle>
        <CardDescription>orthographic · land 1:110m · one revolution per minute</CardDescription>
      </CardHeader>
      <CardContent>
        <div ref={wrapRef} className="aspect-square w-full">
          <canvas ref={canvasRef} role="img" aria-label="Rotating orthographic globe" />
        </div>
      </CardContent>
    </Card>
  );
}

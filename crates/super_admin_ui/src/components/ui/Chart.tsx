import type { ComponentChildren } from 'preact';
import { useId, useState } from 'preact/hooks';
import { niceMax } from '../../lib/format';
import { Panel } from './Page';

export type Series = { name: string; color: string; values: (number | null)[] };

const WIDTH = 1000;

/** A value on the chart's left, over its grid line */
const TICK = 'pointer-events-none absolute left-0 bg-base-100 pr-1 font-mono text-[11px] leading-none text-base-content/45';

type Point = [x: number, y: number];

/** Where the value at `index` of the last `span` values is drawn, newest at the right */
function pointAt(values: (number | null)[], index: number, span: number, max: number, height: number): Point | null {
  const value = values[index];
  if (value === null || value === undefined) return null;
  const x = ((span - values.length + index) / Math.max(1, span - 1)) * WIDTH;
  return [x, height - (Math.min(value, max) / max) * height];
}

/** The runs of values without gaps, as points */
function runs(values: (number | null)[], span: number, max: number, height: number): Point[][] {
  const found: Point[][] = [];
  let run: Point[] = [];
  values.forEach((_, index) => {
    const point = pointAt(values, index, span, max, height);
    if (point) {
      run.push(point);
      return;
    }
    if (run.length) found.push(run);
    run = [];
  });
  if (run.length) found.push(run);
  return found;
}

const at = ([x, y]: Point) => `${x.toFixed(1)},${y.toFixed(1)}`;

/** The values as an SVG line (gaps where there's no value) */
function line(found: Point[][]): string {
  return found.map((run) => `M${run.map(at).join('L')}`).join('');
}

/** The space under the line, down to the chart's bottom */
function area(found: Point[][], height: number): string {
  return found
    .filter((run) => run.length > 1)
    .map((run) => `M${run[0][0].toFixed(1)},${height}L${run.map(at).join('L')}L${run[run.length - 1][0].toFixed(1)},${height}Z`)
    .join('');
}

/** A fill of a series' color, fading to nothing at the bottom */
function Fade({ id, color, strength }: { id: string; color: string; strength: number }) {
  return (
    <linearGradient id={id} x1="0" x2="0" y1="0" y2="1">
      <stop offset="0" stop-color={color} stop-opacity={strength} />
      <stop offset="1" stop-color={color} stop-opacity="0" />
    </linearGradient>
  );
}

/** A dot on a line, with a halo of its color: the latest value, or the one under the pointer */
function Dot({ point, color, height }: { point: Point; color: string; height: number }) {
  return (
    <span
      class="pointer-events-none absolute size-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full"
      style={{
        left: `${(point[0] / WIDTH) * 100}%`,
        top: `${(point[1] / height) * 100}%`,
        background: color,
        boxShadow: `0 0 0 3px color-mix(in oklab, ${color} 22%, transparent), 0 0 10px ${color}`,
      }}
    />
  );
}

type Props = {
  title: string;
  series: Series[];
  /** Points shown (one a second) */
  span: number;
  format: (value: number) => string;
  height?: number;
  /** The chart's top, instead of the largest value */
  max?: number;
};

/** Lines over time, with a legend of the latest values and the values under the pointer */
export function LineChart({ title, series, span, format, height = 80, max }: Props) {
  const [hover, setHover] = useState<number | null>(null);
  const id = useId();
  const shown = series.map((one) => ({ ...one, values: one.values.slice(-span) }));
  const largest = Math.max(0, ...shown.flatMap((one) => one.values.filter((value): value is number => value !== null)));
  const top = max ?? niceMax(largest);
  const length = Math.max(0, ...shown.map((one) => one.values.length));
  // The index under the pointer, in the shown values
  const at = hover === null ? null : Math.round(hover * (span - 1)) - (span - length);
  const indexIn = (values: (number | null)[]) => (at !== null && at >= 0 && at < values.length ? at : values.length - 1);
  const valueAt = (values: (number | null)[]) => values[indexIn(values)] ?? null;
  // One line's fill can be stronger than many overlapping
  const strength = shown.length === 1 ? 0.28 : 0.12;

  return (
    <Panel
      title={title}
      aside={
        <Legend>
          {shown.map((one) => {
            const value = valueAt(one.values);
            return (
              <Swatch color={one.color}>
                {one.name} <LegendValue>{value === null ? '—' : format(value)}</LegendValue>
              </Swatch>
            );
          })}
        </Legend>
      }
    >
      <div
        class="plot relative mx-4 mb-3"
        style={{ height: `${height}px` }}
        onMouseMove={(event) => {
          const box = event.currentTarget.getBoundingClientRect();
          setHover(Math.min(1, Math.max(0, (event.clientX - box.left) / box.width)));
        }}
        onMouseLeave={() => setHover(null)}
      >
        <span class={`${TICK} top-0.5`}>{format(top)}</span>
        <span class={`${TICK} top-[calc(50%+2px)]`}>{format(top / 2)}</span>
        <svg viewBox={`0 0 ${WIDTH} ${height}`} preserveAspectRatio="none" role="img" aria-label={title}>
          {/* The middle and the bottom: a line at the top would read as the header's border */}
          {[0.5, 1].map((share) => (
            <line class="grid" x1="0" x2={WIDTH} y1={height * share} y2={height * share} />
          ))}
          <defs>
            {shown.map((one, index) => (
              <Fade id={`${id}-${index}`} color={one.color} strength={strength} />
            ))}
          </defs>
          {shown.map((one, index) => (
            <path class="area" d={area(runs(one.values, span, top, height), height)} style={{ fill: `url(#${id}-${index})` }} />
          ))}
          {shown.map((one) => (
            <path d={line(runs(one.values, span, top, height))} stroke={one.color} />
          ))}
          {hover !== null && <line class="cursor" x1={hover * WIDTH} x2={hover * WIDTH} y1="0" y2={height} />}
        </svg>
        {shown.map((one) => {
          const point = pointAt(one.values, indexIn(one.values), span, top, height);
          return point && <Dot point={point} color={one.color} height={height} />;
        })}
        {length === 0 && <span class="absolute inset-0 grid place-items-center text-xs text-base-content/45">Waiting for data…</span>}
      </div>
    </Panel>
  );
}

/** What's beside a panel's title: what its lines are, and their latest values */
export function Legend({ children }: { children: ComponentChildren }) {
  return <div class="flex flex-wrap items-center gap-3 text-xs text-base-content/60">{children}</div>;
}

/** A color in a legend, and what it means */
export function Swatch({ color, children }: { color: string; children: ComponentChildren }) {
  return (
    <span class="inline-flex items-center">
      <i class="mr-1.5 inline-block size-2 rounded-xs" style={{ background: color }} />
      {children}
    </span>
  );
}

/** A value in a legend, darker than what names it */
export function LegendValue({ children }: { children: ComponentChildren }) {
  return <b class="ml-0.5 font-semibold text-base-content tabular-nums">{children}</b>;
}

/** A small line of the latest values, for a card */
export function Sparkline({
  values,
  color,
  class: placed = 'absolute inset-x-0 bottom-0 h-10',
}: {
  values: (number | null)[];
  color: string;
  /** Where it's drawn: across the bottom of a card, unless placed otherwise */
  class?: string;
}) {
  // The last 2 minutes, across the card from the first point
  const span = Math.min(120, Math.max(30, values.length));
  const shown = values.slice(-span);
  const top = niceMax(Math.max(0, ...shown.filter((value): value is number => value !== null)));
  const id = useId();
  const found = runs(shown, span, top, 40);
  return (
    <svg class={`sparkline w-full ${placed}`} viewBox={`0 0 ${WIDTH} 40`} preserveAspectRatio="none" aria-hidden="true">
      <defs>
        <Fade id={id} color={color} strength={0.14} />
      </defs>
      <path class="area motion-safe:animate-[fade-in_900ms_ease-out]" d={area(found, 40)} style={{ fill: `url(#${id})` }} />
      <path class="motion-safe:animate-[draw_900ms_ease-out]" pathLength={1} stroke-dasharray="1" d={line(found)} stroke={color} />
    </svg>
  );
}

import { BarChart, HeatmapChart, LineChart } from "echarts/charts";
import { CalendarComponent, GridComponent, LegendComponent, MarkLineComponent, TooltipComponent, VisualMapComponent } from "echarts/components";
import * as echarts from "echarts/core";
import { CanvasRenderer } from "echarts/renderers";
import { useEffect, useRef } from "react";
import { usePalette, type Palette } from "@/lib/colors";

echarts.use([
  LineChart, BarChart, HeatmapChart, GridComponent, TooltipComponent, LegendComponent,
  VisualMapComponent, CalendarComponent, MarkLineComponent, CanvasRenderer,
]);

export type ChartOption = echarts.EChartsCoreOption;

export function baseOption(palette: Palette): ChartOption {
  return {
    animationDuration: 300,
    animationDurationUpdate: 250,
    textStyle: { fontFamily: "inherit", color: palette.ink2, fontSize: 11 },
    tooltip: {
      backgroundColor: palette.surface,
      borderColor: palette.border,
      borderWidth: 1,
      padding: [8, 10],
      textStyle: { color: palette.ink, fontSize: 12 },
      extraCssText: "border-radius:10px;box-shadow:0 8px 24px rgba(0,0,0,.12);",
      confine: true,
    },
  };
}

export function Chart({
  option,
  height,
  className,
  onClick,
  label,
}: {
  option: (palette: Palette) => ChartOption;
  height: number | string;
  className?: string;
  onClick?: (params: { dataIndex: number; seriesIndex?: number; data?: unknown; name?: string }) => void;
  label: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const chart = useRef<echarts.ECharts | null>(null);
  const palette = usePalette();
  const click = useRef(onClick);
  click.current = onClick;

  useEffect(() => {
    if (!ref.current) return;
    const instance = echarts.init(ref.current, undefined, { renderer: "canvas" });
    chart.current = instance;
    instance.on("click", (params) => click.current?.(params as never));
    const observer = new ResizeObserver(() => instance.resize());
    observer.observe(ref.current);
    return () => {
      observer.disconnect();
      instance.dispose();
      chart.current = null;
    };
  }, []);

  useEffect(() => {
    chart.current?.setOption(option(palette), { notMerge: true });
  }, [option, palette]);

  return <div ref={ref} role="img" aria-label={label} className={className} style={{ height, width: "100%" }} />;
}

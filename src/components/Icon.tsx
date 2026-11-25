// Wren's own marks plus the lucide icons the app uses (one import point keeps icon sizes consistent).
import { useId } from "react";

export {
  AppWindow,
  Bold,
  Heading2,
  List,
  ListChecks,
  ArrowDown,
  AudioLines,
  ChevronLeft,
  KeyRound,
  NotebookPen,
  Square,
  Users,
  ArrowLeftRight,
  ArrowRight,
  Ban,
  CalendarDays,
  Check,
  Download,
  Eye,
  Info,
  Lightbulb,
  Mic,
  MessageSquareText,
  Monitor,
  Moon,
  Pause,
  Play,
  Plus,
  Power,
  RotateCcw,
  RotateCw,
  Search,
  ShieldCheck,
  Sun,
  TriangleAlert,
  Volume2,
  WandSparkles,
  Zap,
  ChartColumn,
  Clock,
  CircleAlert,
  Copy,
  FileDown,
  FileText,
  Share,
  Cpu,
  Flame,
  Gauge,
  History,
  House,
  BookA,
  Keyboard,
  MicOff,
  PenLine,
  Settings,
  Sparkles,
  Sunrise,
  Timer,
  Trash2,
  Undo2,
  Wrench,
  X,
} from "lucide-react";

/** The wren silhouette (64-unit grid, same shapes as scripts/assets/wren-icon.svg). */
function Bird({ eye, wing }: { eye: boolean; wing?: string }) {
  const id = useId();
  return (
    <>
      {eye && (
        <mask id={`eye${id}`}>
          <rect width="64" height="64" fill="#fff" />
          <circle cx="45.6" cy="24.8" r="2.4" fill="#000" />
        </mask>
      )}
      <g mask={eye ? `url(#eye${id})` : undefined}>
        <circle cx="30" cy="38" r="15.5" />
        <circle cx="42.5" cy="27" r="9.8" />
        <path d="M33 26 L42 17.5 L45 37 Z" />
        <path d="M51.2 23.4 Q 56 24.6 60 27.6 Q 55.6 28.2 51.2 29.4 Z" />
        <path
          d="M16.5 36 L7.8 15.2 A 3.9 3.9 0 0 1 14.6 11.8 L 27 29 Z"
          stroke="currentColor"
          strokeWidth="1"
          strokeLinejoin="round"
        />
      </g>
      {wing && <path d="M18.5 38.5 Q 27 30.5 40.5 34.5 Q 33.5 46 18.5 38.5 Z" fill={wing} />}
      <path d="M26.5 52 L25 57.5 M34.5 52.5 L34.5 57.5" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </>
  );
}

/** Bird only, in `currentColor`. */
export function WrenMark({ size = 20, eye = true, className }: { size?: number; eye?: boolean; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="4 6 58 54" fill="currentColor" className={className} aria-hidden="true">
      <Bird eye={eye} />
    </svg>
  );
}

/** The app icon: white wren on the indigo tile. */
export function WrenTile({ size = 32 }: { size?: number }) {
  const id = useId();
  return (
    <svg width={size} height={size} viewBox="0 0 1024 1024" aria-hidden="true">
      <defs>
        <linearGradient id={`t${id}`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#5b5ad0" />
          <stop offset="1" stopColor="#36358f" />
        </linearGradient>
      </defs>
      <rect x="32" y="32" width="960" height="960" rx="236" fill={`url(#t${id})`} />
      <g transform="translate(112 112) scale(12.5)" fill="#fff" color="#fff">
        <Bird eye wing="#dedcff" />
      </g>
    </svg>
  );
}

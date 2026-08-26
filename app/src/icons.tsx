// Shared icon layer — Lucide SVG icons with ymux defaults.
//
// One place maps the app's iconography to `lucide-solid` components so sizing
// and stroke stay consistent and the `icon?:` panel-prop chain + dynamic
// lookup maps (notification kinds, provisioning badges) have a single source
// of truth. Prefer importing a named `Icon*` from here over a bare emoji.

import type { ComponentProps, JSX } from "solid-js";
import {
  Activity,
  ArrowDown,
  ArrowLeft,
  ArrowLeftRight,
  ArrowRight,
  ArrowUp,
  ArrowUpRight,
  AtSign,
  Badge,
  BadgePlus,
  Ban,
  Bell,
  Bot,
  Bug,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Circle,
  CircleCheck,
  CircleDot,
  Clipboard,
  Clock,
  Cloud,
  Columns2,
  Container,
  Copy,
  Download,
  Ellipsis,
  ExternalLink,
  Eye,
  EyeOff,
  FileArchive,
  FileText,
  Folder,
  FolderOpen,
  GitBranch,
  GitCompare,
  Globe,
  Hammer,
  History,
  House,
  Info,
  Link,
  Maximize,
  Mic,
  Minimize2,
  NotebookPen,
  PanelRightClose,
  Pencil,
  Plus,
  Power,
  Puzzle,
  RotateCcw,
  RotateCw,
  Rows2,
  Save,
  Scissors,
  Search,
  Settings,
  Smartphone,
  Sparkles,
  Terminal,
  TriangleAlert,
  Trash2,
  Unplug,
  Upload,
  X,
} from "lucide-solid";

type LucideCmp = typeof Bell;
export type IconProps = ComponentProps<LucideCmp>;
export type IconComponent = (props: IconProps) => JSX.Element;

// Wrap a Lucide icon with the ymux default (16px, decorative). Any prop the
// caller passes — size, class, aria-label, stroke-width — overrides the default.
function mk(Cmp: LucideCmp): IconComponent {
  return (props: IconProps) => <Cmp aria-hidden={true} size={16} {...props} />;
}

export const IconActivity = mk(Activity);
export const IconArrowDown = mk(ArrowDown);
export const IconArrowLeft = mk(ArrowLeft);
export const IconArrowLeftRight = mk(ArrowLeftRight);
export const IconArrowRight = mk(ArrowRight);
export const IconArrowUp = mk(ArrowUp);
export const IconArrowUpRight = mk(ArrowUpRight);
export const IconAtSign = mk(AtSign);
export const IconBadge = mk(Badge);
export const IconBadgePlus = mk(BadgePlus);
export const IconBan = mk(Ban);
export const IconBell = mk(Bell);
export const IconBot = mk(Bot);
export const IconBug = mk(Bug);
export const IconCheck = mk(Check);
export const IconChevronDown = mk(ChevronDown);
export const IconChevronLeft = mk(ChevronLeft);
export const IconChevronRight = mk(ChevronRight);
export const IconChevronUp = mk(ChevronUp);
export const IconCircle = mk(Circle);
export const IconCircleCheck = mk(CircleCheck);
export const IconCircleDot = mk(CircleDot);
export const IconClipboard = mk(Clipboard);
export const IconClock = mk(Clock);
export const IconCloud = mk(Cloud);
export const IconColumns = mk(Columns2);
export const IconContainer = mk(Container);
export const IconCopy = mk(Copy);
export const IconDownload = mk(Download);
export const IconExternalLink = mk(ExternalLink);
export const IconEye = mk(Eye);
export const IconEyeOff = mk(EyeOff);
export const IconFileArchive = mk(FileArchive);
export const IconFile = mk(FileText);
export const IconFolder = mk(Folder);
export const IconFolderOpen = mk(FolderOpen);
export const IconGitBranch = mk(GitBranch);
export const IconGitCompare = mk(GitCompare);
export const IconGlobe = mk(Globe);
export const IconHammer = mk(Hammer);
export const IconHistory = mk(History);
export const IconHome = mk(House);
export const IconInfo = mk(Info);
export const IconLink = mk(Link);
export const IconMaximize = mk(Maximize);
export const IconMic = mk(Mic);
export const IconMinimize = mk(Minimize2);
export const IconMore = mk(Ellipsis);
export const IconNotes = mk(NotebookPen);
export const IconPanelClose = mk(PanelRightClose);
export const IconPencil = mk(Pencil);
export const IconPlus = mk(Plus);
export const IconPower = mk(Power);
export const IconPuzzle = mk(Puzzle);
export const IconRefresh = mk(RotateCw);
export const IconRefreshCcw = mk(RotateCcw);
export const IconRows = mk(Rows2);
export const IconSave = mk(Save);
export const IconScissors = mk(Scissors);
export const IconSearch = mk(Search);
export const IconSettings = mk(Settings);
export const IconSmartphone = mk(Smartphone);
export const IconSparkles = mk(Sparkles);
export const IconTerminal = mk(Terminal);
export const IconTrash = mk(Trash2);
export const IconUnplug = mk(Unplug);
export const IconUpload = mk(Upload);
export const IconWarning = mk(TriangleAlert);
export const IconClose = mk(X);

/**
 * Dynamic lookup for places that pick an icon by a runtime string (notification
 * kinds, provisioning-step states). Callers render e.g.
 * `{(iconByName[kind] ?? IconBell)({ size: 14 })}`.
 */
export const iconByName: Record<string, IconComponent> = {
  // notification kinds
  agent: IconBot,
  notify: IconBell,
  error: IconBan,
  build: IconHammer,
  warning: IconWarning,
  // provisioning / status
  ok: IconCircleCheck,
  fail: IconClose,
  pending: IconCircle,
  running: IconCircleDot,
};

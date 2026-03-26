import React from "react";

export const SESSION_ICON_KEYS = [
  "desktop",
  "linux",
  "windows",
  "apple",
  "switch",
  "router",
  "firewall",
  "database",
  "web",
  "cloud",
  "container",
  "printer",
  "lock",
] as const;

export type SessionIcon = (typeof SESSION_ICON_KEYS)[number];

type SvgProps = React.SVGProps<SVGSVGElement>;

const svgDefaults: SvgProps = {
  xmlns: "http://www.w3.org/2000/svg",
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.5,
  strokeLinecap: "round",
  strokeLinejoin: "round",
};

function Svg({ children, ...props }: SvgProps): React.JSX.Element {
  return (
    <svg {...svgDefaults} {...props}>
      {children}
    </svg>
  );
}

/** Monitor with stand. */
function DesktopIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <line x1="8" y1="21" x2="16" y2="21" />
      <line x1="12" y1="17" x2="12" y2="21" />
    </Svg>
  );
}

/** Simplified Tux penguin. */
function LinuxIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <ellipse cx="12" cy="14" rx="6" ry="8" />
      <ellipse cx="12" cy="7" rx="4" ry="4.5" />
      <circle cx="10" cy="6.5" r="0.8" fill="currentColor" stroke="none" />
      <circle cx="14" cy="6.5" r="0.8" fill="currentColor" stroke="none" />
      <path d="M10.5 8.5 Q12 10 13.5 8.5" />
      <path d="M6 18 L4 21" />
      <path d="M18 18 L20 21" />
    </Svg>
  );
}

/** Four-pane window. */
function WindowsIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="3" y="3" width="18" height="18" rx="1" />
      <line x1="12" y1="3" x2="12" y2="21" />
      <line x1="3" y1="12" x2="21" y2="12" />
    </Svg>
  );
}

/** Apple with leaf. */
function AppleIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <path d="M12 3 Q14 1 16 3" />
      <path d="M12 3 C7 5 4 10 5 15 C6 20 9 22 12 22 C15 22 18 20 19 15 C20 10 17 5 12 3Z" />
    </Svg>
  );
}

/** Cisco-style switch: rounded rectangle with bidirectional arrow pairs. */
function SwitchIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="2" y="5" width="20" height="14" rx="2" />
      {/* Top arrow pair: left-pointing and right-pointing */}
      <polygon points="4,9 8,7 8,11" fill="currentColor" stroke="none" />
      <polygon points="20,9 16,7 16,11" fill="currentColor" stroke="none" />
      {/* Bottom arrow pair */}
      <polygon points="4,15 8,13 8,17" fill="currentColor" stroke="none" />
      <polygon points="20,15 16,13 16,17" fill="currentColor" stroke="none" />
    </Svg>
  );
}

/** Cisco-style router: circle with four outward arrows at cardinal directions. */
function RouterIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <circle cx="12" cy="12" r="10" />
      {/* Four inward-pointing arrows */}
      <polygon points="12,2 9,7 15,7" fill="currentColor" stroke="none" />
      <polygon points="12,22 9,17 15,17" fill="currentColor" stroke="none" />
      <polygon points="2,12 7,9 7,15" fill="currentColor" stroke="none" />
      <polygon points="22,12 17,9 17,15" fill="currentColor" stroke="none" />
    </Svg>
  );
}

/** Cisco-style firewall: brick wall with vertical lines. */
function FirewallIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="3" y="3" width="18" height="18" rx="1" />
      <line x1="3" y1="8" x2="21" y2="8" />
      <line x1="3" y1="13" x2="21" y2="13" />
      <line x1="3" y1="18" x2="21" y2="18" />
      <line x1="8" y1="3" x2="8" y2="8" />
      <line x1="16" y1="3" x2="16" y2="8" />
      <line x1="12" y1="8" x2="12" y2="13" />
      <line x1="8" y1="13" x2="8" y2="18" />
      <line x1="16" y1="13" x2="16" y2="18" />
      <line x1="12" y1="18" x2="12" y2="21" />
    </Svg>
  );
}

/** Stacked cylinders. */
function DatabaseIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <ellipse cx="12" cy="5" rx="8" ry="3" />
      <path d="M4 5 V19 C4 20.7 7.6 22 12 22 C16.4 22 20 20.7 20 19 V5" />
      <path d="M4 12 C4 13.7 7.6 15 12 15 C16.4 15 20 13.7 20 12" />
    </Svg>
  );
}

/** Globe with latitude/longitude lines. */
function WebIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <circle cx="12" cy="12" r="10" />
      <ellipse cx="12" cy="12" rx="4" ry="10" />
      <line x1="2" y1="12" x2="22" y2="12" />
      <path d="M4 7 Q12 9 20 7" />
      <path d="M4 17 Q12 15 20 17" />
    </Svg>
  );
}

/** Cloud shape. */
function CloudIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <path d="M6 19 C2 19 2 14 5 13 C4 8 10 6 13 9 C15 6 21 7 20 12 C23 13 22 19 18 19Z" />
    </Svg>
  );
}

/** Container / stacked boxes. */
function ContainerIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="3" y="3" width="18" height="6" rx="1" />
      <rect x="3" y="15" width="18" height="6" rx="1" />
      <line x1="7" y1="6" x2="7" y2="6" strokeWidth="2" />
      <line x1="7" y1="18" x2="7" y2="18" strokeWidth="2" />
      <line x1="12" y1="9" x2="12" y2="15" />
    </Svg>
  );
}

/** Printer with paper tray. */
function PrinterIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="6" y="2" width="12" height="6" rx="1" />
      <rect x="3" y="8" width="18" height="9" rx="1" />
      <rect x="7" y="14" width="10" height="8" rx="1" />
      <circle cx="17" cy="11" r="1" fill="currentColor" stroke="none" />
    </Svg>
  );
}

/** Padlock. */
function LockIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <rect x="5" y="11" width="14" height="10" rx="2" />
      <path d="M8 11 V7 C8 4 10 2 12 2 C14 2 16 4 16 7 V11" />
      <circle cx="12" cy="16" r="1.5" fill="currentColor" stroke="none" />
    </Svg>
  );
}

/** Folder icon (for the tree view). */
export function FolderIcon(props: SvgProps): React.JSX.Element {
  return (
    <Svg {...props}>
      <path d="M2 6 C2 5 3 4 4 4 L10 4 L12 6 L20 6 C21 6 22 7 22 8 L22 18 C22 19 21 20 20 20 L4 20 C3 20 2 19 2 18Z" />
    </Svg>
  );
}

const ICON_MAP: Record<SessionIcon, React.FC<SvgProps>> = {
  desktop: DesktopIcon,
  linux: LinuxIcon,
  windows: WindowsIcon,
  apple: AppleIcon,
  switch: SwitchIcon,
  router: RouterIcon,
  firewall: FirewallIcon,
  database: DatabaseIcon,
  web: WebIcon,
  cloud: CloudIcon,
  container: ContainerIcon,
  printer: PrinterIcon,
  lock: LockIcon,
};

/** Renders the SVG icon for a session icon key. Falls back to DesktopIcon. */
export function SessionIconComponent({
  iconKey,
  ...props
}: { iconKey: string } & SvgProps): React.JSX.Element {
  const key = iconKey as SessionIcon;
  const Icon = key in ICON_MAP ? ICON_MAP[key] : ICON_MAP.desktop;
  return <Icon {...props} />;
}

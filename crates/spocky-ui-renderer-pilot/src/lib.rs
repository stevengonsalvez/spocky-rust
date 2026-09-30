use dioxus::prelude::*;

pub const APP_TITLE: &str = "Spocky";

const SHELL_CSS: &str = r#"
:root { color-scheme: light; font-family: system-ui, -apple-system, "system-ui", "Segoe UI", Roboto, Helvetica, Arial, sans-serif; -webkit-font-smoothing: antialiased; -moz-osx-font-smoothing: grayscale; }
* { box-sizing: border-box; }
body { margin: 0; background: #fff; color: #1a1a1e; }
button, a { font: inherit; }
.shell { min-height: 100vh; display: grid; grid-template-columns: 320px 1fr; background: #fff; }
.sidebar { min-height: 100vh; display: flex; flex-direction: column; border-right: 1px solid #e4e4e7; background: #f4f4f5; color: #71717a; font-size: 14px; }
.nav-list { display: grid; gap: 2px; padding: 8px 0 6px; border-bottom: 1px solid #e4e4e7; }
.nav-button { min-height: 28px; display: flex; align-items: center; gap: 8px; padding: 4px 8px; border: 0; border-radius: 8px; color: inherit; background: transparent; text-align: left; cursor: pointer; }
.nav-button:hover, .nav-button:focus-visible { background: #ececf0; color: #28292f; outline: 2px solid #8ba9d8; outline-offset: -2px; }
.nav-list > .nav-button { margin: 0 8px; }
.nav-icon { display: block; width: 14px; height: 14px; flex: none; color: #71717a; }
.nav-icon svg, .footer-icon svg { display: block; width: 100%; height: 100%; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
.sidebar-scroll { flex: 1; overflow-y: auto; transform: translateZ(0); }
.sidebar-list-content { min-height: 100%; padding: 2px 8px 16px; }
.sidebar-empty { margin: 12px 0 0; padding: 16px; display: flex; flex-direction: column; border: 1px solid #e4e4e7; border-radius: 8px; color: #000; font-size: 16px; }
.sidebar-empty-copy { display: flex; flex-direction: column; gap: 4px; }
.sidebar-empty-title { margin: 0; color: #1a1a1e; font-size: 12px; line-height: normal; }
.sidebar-empty-detail { margin: 0; color: #71717a; font-size: 12px; line-height: normal; }
.sidebar-empty-actions { display: flex; gap: 8px; margin-top: 16px; }
.sidebar-empty-actions button { min-height: 28px; display: flex; align-items: center; gap: 8px; padding: 0 12px; border: 1px solid #e4e4e7; border-radius: 12px; background: #e4e4e7; color: #1a1a1e; font-size: 12px; cursor: pointer; }
.sidebar-empty-actions button:last-child { border-color: #ececf1; background: transparent; }
.sidebar-empty-actions .nav-icon { width: 12px; height: 12px; color: currentColor; }
.sidebar-footer { min-height: 57px; display: flex; align-items: center; gap: 8px; padding: 12px 8px; border-top: 1px solid #e4e4e7; }
.sidebar-footer .nav-button:first-child { min-width: 0; min-height: 32px; flex: 1; padding: 6px 8px; }
.footer-icon { display: block; width: 16px; height: 16px; flex: none; color: #71717a; }
.sidebar-footer .nav-button:first-child .footer-icon, .sidebar-footer .nav-button:nth-child(2) .footer-icon { width: 14px; height: 14px; }
.icon-button { width: 28px; min-width: 28px; height: 28px; min-height: 0; justify-content: center; padding: 4px; }
.mobile-menu { display: flex; position: absolute; z-index: 2; top: 4.5px; left: 324px; width: 26px; height: 26px; align-items: center; justify-content: center; padding: 0; border: 0; border-radius: 6px; background: transparent; color: #71717a; cursor: pointer; }
.desktop-menu-icon { display: block; width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
.mobile-menu-icon { position: relative; width: 16px; height: 12px; display: none; }
.mobile-menu-line { position: absolute; top: 0; left: 0; width: 16px; height: 1px; border-radius: 999px; background: currentColor; }
.mobile-menu-line:nth-child(2) { top: 5px; }
.mobile-menu-line.short { top: 10px; width: 8px; height: 2px; }
.workspace { position: relative; min-width: 0; min-height: 100vh; padding: 198px 24px 72px; }
.content { width: min(452px, 100%); margin: 0 auto; }
.mark { width: 52px; height: 52px; margin: 0 auto 73.5px; }
.mark svg { display: block; width: 52px; height: 52px; fill: #1a1a1e; transform: translateY(-6px); }
.actions { display: flex; flex-flow: row wrap; justify-content: flex-start; gap: 12px; }
.action { width: 220px; min-height: 132px; display: flex; flex-direction: column; gap: 12px; padding: 16px; border: 1px solid #e4e4e7; border-radius: 12px; background: #fafafa; color: #000; text-align: left; cursor: pointer; }
.action:hover, .action:focus-visible { border-color: #aeb0b9; box-shadow: 0 1px 4px rgb(28 29 34 / 10%); outline: 2px solid #8ba9d8; outline-offset: 2px; }
.action-icon { display: block; width: 20px; height: 20px; color: #71717a; }
.action:first-child .action-icon { color: #20744a; }
.action-icon svg { display: block; width: 20px; height: 20px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
.action-copy { display: flex; flex-direction: column; gap: 4px; }
.action-title { display: block; font-size: 14px; line-height: normal; color: #1a1a1e; }
.action-detail { display: block; color: #71717a; font-size: 14px; line-height: 18px; }
.community { position: absolute; right: 0; bottom: 44px; left: 0; display: flex; justify-content: center; gap: 0; color: #71717a; font-size: 14px; }
.community a { min-height: 32px; display: flex; align-items: center; justify-content: center; gap: 8px; padding: 0 12px; border: 1px solid transparent; border-radius: 12px; color: inherit; text-decoration: none; }
.community-icon, .community-icon svg { display: block; width: 14px; height: 14px; flex: none; }
.community-icon-fill svg { fill: currentColor; }
.community-icon-stroke svg { fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
.community a:hover, .community a:focus-visible { color: #292a30; text-decoration: underline; outline: none; }
.dialog-overlay { position: fixed; z-index: 10; inset: 0; display: flex; justify-content: center; align-items: flex-start; padding-top: 48px; background: rgb(0 0 0 / 50%); }
.dialog-panel { width: min(560px, calc(100% - 32px)); max-height: calc(100vh - 96px); display: flex; flex-direction: column; overflow: hidden; border: 1px solid #e4e4e7; border-radius: 12px; background: #fff; color: #1a1a1e; }
.dialog-header { padding: 16px; border-bottom: 1px solid #e4e4e7; }
.dialog-title { font-size: 16px; font-weight: 600; }
.dialog-host { margin-top: 2px; color: #71717a; font-size: 12px; }
.dialog-results { display: grid; gap: 4px; padding: 8px; }
.dialog-row { display: flex; flex-direction: column; gap: 2px; padding: 10px 12px; border-radius: 8px; outline: none; }
.dialog-row:first-child, .dialog-row:focus-visible { background: #ececf0; }
.dialog-row-title { font-size: 14px; }
.dialog-row-detail { color: #71717a; font-size: 12px; }
.dialog-footer { display: flex; gap: 16px; padding: 12px 16px; border-top: 1px solid #e4e4e7; color: #71717a; font-size: 12px; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
@media (max-width: 42rem) {
  .shell { display: block; }
  .sidebar { display: none; }
  .sidebar-empty { display: none; }
  .mobile-menu { top: 19.5px; left: 8px; width: 32px; height: 32px; border-radius: 6px; }
  .desktop-menu-icon { display: none; }
  .mobile-menu-icon { display: block; }
  .workspace { min-height: 100vh; padding: 114px 24px 92px; }
  .content { width: 100%; }
  .mark { margin-bottom: 58px; }
  .mark svg { transform: translateY(-2px); }
  .actions { margin-top: -4px; }
  .action { width: 100%; min-height: 0; padding: 16px; }
  .community { bottom: 72px; }
}
"#;

#[derive(Clone, Copy)]
struct ProjectAction {
    icon: ProjectIcon,
    title: &'static str,
    detail: &'static str,
}

#[derive(Clone, Copy)]
enum ProjectIcon {
    FolderOpen,
    Inbox,
    Plug,
}

#[derive(Clone, Copy)]
enum SidebarIcon {
    Plus,
    History,
    Search,
    CalendarClock,
    FolderPlus,
    Server,
    Import,
    Gauge,
    CircleHelp,
    Settings,
}

const PROJECT_ACTIONS: [ProjectAction; 3] = [
    ProjectAction {
        icon: ProjectIcon::FolderOpen,
        title: "Add a project",
        detail: "Open a folder on your machine",
    },
    ProjectAction {
        icon: ProjectIcon::Inbox,
        title: "Import session",
        detail: "Open a Claude Code, Codex or other session you started in a terminal",
    },
    ProjectAction {
        icon: ProjectIcon::Plug,
        title: "Setup providers",
        detail: "Configure Claude Code, Codex, and more",
    },
];

// Retained only for original visual-baseline comparison until Spocky artwork is approved.
const BASELINE_LOGO_PATH: &str = "M291.495 91.399C333.897 104.892 379.155 135.075 416.229 173.191C453.389 211.394 484.429 259.725 495.708 311.251C497.555 319.693 498.865 328.216 499.586 336.776C509.755 326.554 519.867 317.815 529.89 311.547C540.647 304.821 553.808 299.297 568.641 299.785C584.29 300.299 597.395 307.326 607.747 317.632C632.173 341.947 629.612 372.898 619.872 397.936C610.185 422.833 591.557 447.826 572.732 469.124C553.591 490.78 532.713 510.308 516.779 524.318C508.775 531.355 501.936 537.073 497.07 541.052C494.635 543.043 492.689 544.603 491.334 545.679C490.657 546.217 490.126 546.635 489.756 546.926C489.571 547.071 489.425 547.184 489.321 547.265C489.269 547.305 489.227 547.338 489.196 547.362C489.181 547.374 489.168 547.385 489.157 547.393C489.153 547.397 489.147 547.401 489.144 547.403C489.134 547.4 488.837 547.06 473.001 528.499L489.135 547.411C478.157 555.911 462.033 554.334 453.122 543.89C444.213 533.448 445.887 518.094 456.861 509.592C456.863 509.591 456.865 509.588 456.869 509.586C456.88 509.577 456.902 509.561 456.933 509.536C456.997 509.487 457.101 509.404 457.245 509.292C457.533 509.066 457.979 508.715 458.569 508.247C459.749 507.31 461.506 505.901 463.742 504.073C468.216 500.414 474.589 495.088 482.073 488.508C497.114 475.284 516.315 457.282 533.578 437.75C551.157 417.862 565.26 398.01 571.859 381.048C578.403 364.227 575.681 356.302 570.724 351.367C568.928 349.579 567.744 348.902 567.267 348.676C566.888 348.496 566.811 348.52 566.804 348.52C566.605 348.513 563.971 348.537 557.953 352.3C545.161 360.299 528.815 377.492 506.807 403.867C494.927 418.106 481.871 434.435 467.547 451.957C463.709 457.28 459.503 462.538 454.91 467.717L454.702 467.549C420.808 508.347 380.37 553.856 332.335 593.848C301.853 619.226 262.656 622.597 228.642 614.743C194.834 606.936 162.658 587.448 142.217 561.686C108.054 518.631 100.57 469.801 108.223 427.836C115.56 387.606 137.391 351.005 166.502 331.557C161.248 315.813 156.813 299.49 153.519 283.013C142.593 228.368 143.239 167.031 174.28 119.619C186.922 100.31 205.846 89.1535 227.387 85.2773C248.1 81.5504 270.278 84.648 291.495 91.399ZM378.642 206.356C345.773 172.563 307.463 147.917 275.208 137.654C259.096 132.527 246.171 131.514 236.828 133.195C228.314 134.727 222.227 138.497 217.721 145.38C196.712 177.468 193.858 224.004 203.82 273.827C206.532 287.394 210.127 300.834 214.345 313.817C236.45 310.276 260.156 311.463 281.22 317.11C319.621 327.403 357.501 355.419 357.501 405.654C357.501 435.255 339.111 465.136 307.278 473.815C273.211 483.103 238.854 464.822 213.105 427.541C203.716 413.947 194.443 397.766 185.947 379.89C174.028 392.223 163.08 411.953 158.673 436.118C153.128 466.518 158.514 501.286 183.085 532.253C195.993 548.522 217.742 562.031 240.771 567.349C263.594 572.619 284.147 569.24 298.664 557.154C349.383 514.927 390.709 466.547 426.366 422.952C448.879 390.86 453.195 356.06 445.578 321.265C436.703 280.718 411.425 240.06 378.642 206.356ZM306.296 405.722C306.296 384.769 292.223 370.736 267.284 364.051C256.012 361.03 244.156 360.087 233.095 360.771C240.361 375.935 248.168 389.513 255.897 400.704C275.647 429.298 289.989 427.822 293.247 426.934C298.737 425.437 306.296 418.161 306.296 405.722Z";

const GITHUB_ICON_PATH: &str = "m12.301 0h.093c2.242 0 4.34.613 6.137 1.68l-.055-.031c1.871 1.094 3.386 2.609 4.449 4.422l.031.058c1.04 1.769 1.654 3.896 1.654 6.166 0 5.406-3.483 10-8.327 11.658l-.087.026c-.063.02-.135.031-.209.031-.162 0-.312-.054-.433-.144l.002.001c-.128-.115-.208-.281-.208-.466 0-.005 0-.01 0-.014v.001q0-.048.008-1.226t.008-2.154c.007-.075.011-.161.011-.249 0-.792-.323-1.508-.844-2.025.618-.061 1.176-.163 1.718-.305l-.076.017c.573-.16 1.073-.373 1.537-.642l-.031.017c.508-.28.938-.636 1.292-1.058l.006-.007c.372-.476.663-1.036.84-1.645l.009-.035c.209-.683.329-1.468.329-2.281 0-.045 0-.091-.001-.136v.007c0-.022.001-.047.001-.072 0-1.248-.482-2.383-1.269-3.23l.003.003c.168-.44.265-.948.265-1.479 0-.649-.145-1.263-.404-1.814l.011.026c-.115-.022-.246-.035-.381-.035-.334 0-.649.078-.929.216l.012-.005c-.568.21-1.054.448-1.512.726l.038-.022-.609.384c-.922-.264-1.981-.416-3.075-.416s-2.153.152-3.157.436l.081-.02q-.256-.176-.681-.433c-.373-.214-.814-.421-1.272-.595l-.066-.022c-.293-.154-.64-.244-1.009-.244-.124 0-.246.01-.364.03l.013-.002c-.248.524-.393 1.139-.393 1.788 0 .531.097 1.04.275 1.509l-.01-.029c-.785.844-1.266 1.979-1.266 3.227 0 .025 0 .051.001.076v-.004c-.001.039-.001.084-.001.13 0 .809.12 1.591.344 2.327l-.015-.057c.189.643.476 1.202.85 1.693l-.009-.013c.354.435.782.793 1.267 1.062l.022.011c.432.252.933.465 1.46.614l.046.011c.466.125 1.024.227 1.595.284l.046.004c-.431.428-.718 1-.784 1.638l-.001.012c-.207.101-.448.183-.699.236l-.021.004c-.256.051-.549.08-.85.08-.022 0-.044 0-.066 0h.003c-.394-.008-.756-.136-1.055-.348l.006.004c-.371-.259-.671-.595-.881-.986l-.007-.015c-.198-.336-.459-.614-.768-.827l-.009-.006c-.225-.169-.49-.301-.776-.38l-.016-.004-.32-.048c-.023-.002-.05-.003-.077-.003-.14 0-.273.028-.394.077l.007-.003q-.128.072-.08.184c.039.086.087.16.145.225l-.001-.001c.061.072.13.135.205.19l.003.002.112.08c.283.148.516.354.693.603l.004.006c.191.237.359.505.494.792l.01.024.16.368c.135.402.38.738.7.981l.005.004c.3.234.662.402 1.057.478l.016.002c.33.064.714.104 1.106.112h.007c.045.002.097.002.15.002.261 0 .517-.021.767-.062l-.027.004.368-.064q0 .609.008 1.418t.008.873v.014c0 .185-.08.351-.208.466h-.001c-.119.089-.268.143-.431.143-.075 0-.147-.011-.214-.032l.005.001c-4.929-1.689-8.409-6.283-8.409-11.69 0-2.268.612-4.393 1.681-6.219l-.032.058c1.094-1.871 2.609-3.386 4.422-4.449l.058-.031c1.739-1.034 3.835-1.645 6.073-1.645h.098-.005zm-7.64 17.666q.048-.112-.112-.192-.16-.048-.208.032-.048.112.112.192.144.096.208-.032zm.497.545q.112-.08-.032-.256-.16-.144-.256-.048-.112.08.032.256.159.157.256.047zm.48.72q.144-.112 0-.304-.128-.208-.272-.096-.144.08 0 .288t.272.112zm.672.673q.128-.128-.064-.304-.192-.192-.32-.048-.144.128.064.304.192.192.32.044zm.913.4q.048-.176-.208-.256-.24-.064-.304.112t.208.24q.24.097.304-.096zm1.009.08q0-.208-.272-.176-.256 0-.256.176 0 .208.272.176.256.001.256-.175zm.929-.16q-.032-.176-.288-.144-.256.048-.224.24t.288.128.225-.224z";
const HEART_ICON_PATH: &str = "M2 9.5a5.5 5.5 0 0 1 9.591-3.676.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5";
const DISCORD_ICON_PATH: &str = "M20.317 4.3698a19.7913 19.7913 0 00-4.8851-1.5152.0741.0741 0 00-.0785.0371c-.211.3753-.4447.8648-.6083 1.2495-1.8447-.2762-3.68-.2762-5.4868 0-.1636-.3933-.4058-.8742-.6177-1.2495a.077.077 0 00-.0785-.037 19.7363 19.7363 0 00-4.8852 1.515.0699.0699 0 00-.0321.0277C.5334 9.0458-.319 13.5799.0992 18.0578a.0824.0824 0 00.0312.0561c2.0528 1.5076 4.0413 2.4228 5.9929 3.0294a.0777.0777 0 00.0842-.0276c.4616-.6304.8731-1.2952 1.226-1.9942a.076.076 0 00-.0416-.1057c-.6528-.2476-1.2743-.5495-1.8722-.8923a.077.077 0 01-.0076-.1277c.1258-.0943.2517-.1923.3718-.2914a.0743.0743 0 01.0776-.0105c3.9278 1.7933 8.18 1.7933 12.0614 0a.0739.0739 0 01.0785.0095c.1202.099.246.1981.3728.2924a.077.077 0 01-.0066.1276 12.2986 12.2986 0 01-1.873.8914.0766.0766 0 00-.0407.1067c.3604.698.7719 1.3628 1.225 1.9932a.076.076 0 00.0842.0286c1.961-.6067 3.9495-1.5219 6.0023-3.0294a.077.077 0 00.0313-.0552c.5004-5.177-.8382-9.6739-3.5485-13.6604a.061.061 0 00-.0312-.0286zM8.02 15.3312c-1.1825 0-2.1569-1.0857-2.1569-2.419 0-1.3332.9555-2.4189 2.157-2.4189 1.2108 0 2.1757 1.0952 2.1568 2.419 0 1.3332-.9555 2.4189-2.1569 2.4189zm7.9748 0c-1.1825 0-2.1569-1.0857-2.1569-2.419 0-1.3332.9554-2.4189 2.1569-2.4189 1.2108 0 2.1757 1.0952 2.1568 2.419 0 1.3332-.946 2.4189-2.1568 2.4189Z";

fn project_icon(icon: ProjectIcon) -> Element {
    match icon {
        ProjectIcon::FolderOpen => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" }
            }
        },
        ProjectIcon::Inbox => rsx! {
            svg { view_box: "0 0 24 24",
                polyline { points: "22 12 16 12 14 15 10 15 8 12 2 12" }
                path { d: "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" }
            }
        },
        ProjectIcon::Plug => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M12 22v-5" }
                path { d: "M9 8V2" }
                path { d: "M15 8V2" }
                path { d: "M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z" }
            }
        },
    }
}

fn sidebar_icon(icon: SidebarIcon) -> Element {
    match icon {
        SidebarIcon::Plus => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M5 12h14" }
                path { d: "M12 5v14" }
            }
        },
        SidebarIcon::History => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" }
                path { d: "M3 3v5h5" }
                path { d: "M12 7v5l4 2" }
            }
        },
        SidebarIcon::Search => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "m21 21-4.34-4.34" }
                circle { cx: "11", cy: "11", r: "8" }
            }
        },
        SidebarIcon::CalendarClock => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M16 14v2.2l1.6 1" }
                path { d: "M16 2v4" }
                path { d: "M21 7.5V6a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h3.5" }
                path { d: "M3 10h5" }
                path { d: "M8 2v4" }
                circle { cx: "16", cy: "16", r: "6" }
            }
        },
        SidebarIcon::FolderPlus => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M12 10v6" }
                path { d: "M9 13h6" }
                path { d: "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" }
            }
        },
        SidebarIcon::Server => rsx! {
            svg { view_box: "0 0 24 24",
                rect { width: "20", height: "8", x: "2", y: "2", rx: "2", ry: "2" }
                rect { width: "20", height: "8", x: "2", y: "14", rx: "2", ry: "2" }
                line { x1: "6", x2: "6.01", y1: "6", y2: "6" }
                line { x1: "6", x2: "6.01", y1: "18", y2: "18" }
            }
        },
        SidebarIcon::Import => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M12 3v12" }
                path { d: "m8 11 4 4 4-4" }
                path { d: "M8 5H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-4" }
            }
        },
        SidebarIcon::Gauge => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "m12 14 4-4" }
                path { d: "M3.34 19a10 10 0 1 1 17.32 0" }
            }
        },
        SidebarIcon::CircleHelp => rsx! {
            svg { view_box: "0 0 24 24",
                circle { cx: "12", cy: "12", r: "10" }
                path { d: "M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" }
                path { d: "M12 17h.01" }
            }
        },
        SidebarIcon::Settings => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915" }
                circle { cx: "12", cy: "12", r: "3" }
            }
        },
    }
}

/// Interactive open-project shell shared by web, desktop, and mobile renderers.
///
/// # Errors
///
/// Dioxus returns a rendering error when component construction fails.
#[allow(non_snake_case)]
pub fn SpockyShell() -> Element {
    let mut add_project_open = use_signal(|| false);

    rsx! {
        style { {SHELL_CSS} }
        main { class: "shell",
            nav { class: "sidebar", aria_label: "Primary navigation",
                div { class: "nav-list",
                    button { class: "nav-button", aria_label: "New workspace", span { class: "nav-icon", {sidebar_icon(SidebarIcon::Plus)} } "New workspace" }
                    button { class: "nav-button", aria_label: "History", span { class: "nav-icon", {sidebar_icon(SidebarIcon::History)} } "History" }
                    button { class: "nav-button", aria_label: "Search", span { class: "nav-icon", {sidebar_icon(SidebarIcon::Search)} } "Search" }
                    button { class: "nav-button", aria_label: "Schedules", span { class: "nav-icon", {sidebar_icon(SidebarIcon::CalendarClock)} } "Schedules" }
                }
                div { class: "sidebar-scroll",
                    div { class: "sidebar-list-content",
                        div { class: "sidebar-empty",
                            div { class: "sidebar-empty-copy",
                                div { class: "sidebar-empty-title", "No projects yet" }
                                div { class: "sidebar-empty-detail", "Add a project to get started" }
                            }
                            div { class: "sidebar-empty-actions",
                                button { span { class: "nav-icon", {sidebar_icon(SidebarIcon::Plus)} } div { "Add project" } }
                                button { span { class: "nav-icon", {sidebar_icon(SidebarIcon::Import)} } div { "Import session" } }
                            }
                        }
                    }
                }
                div { class: "sidebar-footer",
                    button { class: "nav-button", span { class: "footer-icon", {sidebar_icon(SidebarIcon::FolderPlus)} } "Add project" }
                    button { class: "nav-button icon-button", aria_label: "Hosts", span { class: "footer-icon", {sidebar_icon(SidebarIcon::Server)} } }
                    button { class: "nav-button icon-button", aria_label: "Import", span { class: "footer-icon", {sidebar_icon(SidebarIcon::Import)} } }
                    button { class: "nav-button icon-button", aria_label: "Activity", span { class: "footer-icon", {sidebar_icon(SidebarIcon::Gauge)} } }
                    button { class: "nav-button icon-button", aria_label: "Help", span { class: "footer-icon", {sidebar_icon(SidebarIcon::CircleHelp)} } }
                    button { class: "nav-button icon-button", aria_label: "Settings", span { class: "footer-icon", {sidebar_icon(SidebarIcon::Settings)} } }
                }
            }
            button { class: "mobile-menu", aria_label: "Open menu",
                svg { class: "desktop-menu-icon", view_box: "0 0 24 24",
                    rect { width: "18", height: "18", x: "3", y: "3", rx: "2" }
                    path { d: "M9 3v18" }
                }
                span { class: "mobile-menu-icon", aria_hidden: "true",
                    span { class: "mobile-menu-line" }
                    span { class: "mobile-menu-line" }
                    span { class: "mobile-menu-line short" }
                }
            }
            section { class: "workspace", aria_label: "Open project",
                div { class: "content",
                    div { class: "mark", aria_hidden: "true",
                        svg { view_box: "0 0 700 700", path { d: BASELINE_LOGO_PATH } }
                    }
                    div { class: "actions",
                        for (index, action) in PROJECT_ACTIONS.iter().enumerate() {
                            div {
                                class: "action",
                                role: "button",
                                tabindex: "0",
                                onclick: move |_| {
                                    if index == 0 {
                                        add_project_open.set(true);
                                    }
                                },
                                onkeydown: move |event: KeyboardEvent| {
                                    if index == 0 && event.key() == Key::Enter {
                                        add_project_open.set(true);
                                    }
                                },
                                span { class: "action-icon", {project_icon(action.icon)} }
                                div { class: "action-copy",
                                    div { class: "action-title", "{action.title}" }
                                    div { class: "action-detail", "{action.detail}" }
                                }
                            }
                        }
                    }
                }
                footer { class: "community",
                    a { href: "https://github.com/getpaseo/paseo", span { class: "community-icon community-icon-fill", aria_hidden: "true", svg { view_box: "0 -0.5 25 25", path { d: GITHUB_ICON_PATH } } } "Star" }
                    a { href: "https://github.com/sponsors/boudra", span { class: "community-icon community-icon-stroke", aria_hidden: "true", svg { view_box: "0 0 24 24", path { d: HEART_ICON_PATH } } } "Sponsor" }
                    a { href: "https://discord.gg/jz8T2uahpH", span { class: "community-icon community-icon-fill", aria_hidden: "true", svg { view_box: "0 0 24 24", path { d: DISCORD_ICON_PATH } } } "Community" }
                }
                span { class: "sr-only", {APP_TITLE} }
            }
            if add_project_open() {
                AddProjectDialog {}
            }
        }
    }
}

#[allow(non_snake_case)]
fn AddProjectDialog() -> Element {
    rsx! {
        div {
            class: "dialog-overlay",
            role: "dialog",
            aria_modal: "true",
            aria_label: "Add project: method",
            div { class: "dialog-panel",
                div { class: "dialog-header",
                    div { class: "dialog-title", "Add project" }
                    div { class: "dialog-host", "isolated-baseline" }
                }
                div { class: "dialog-results",
                    div { class: "dialog-row", role: "button", tabindex: "0", aria_selected: "true",
                        div { class: "dialog-row-title", "Search for directory" }
                        div { class: "dialog-row-detail", "Find a directory on isolated-baseline" }
                    }
                    div { class: "dialog-row", role: "button", tabindex: "0", aria_selected: "false",
                        div { class: "dialog-row-title", "Clone from GitHub" }
                        div { class: "dialog-row-detail", "Enter a GitHub URL or owner/repo" }
                    }
                    div { class: "dialog-row", role: "button", tabindex: "0", aria_selected: "false",
                        div { class: "dialog-row-title", "New directory" }
                        div { class: "dialog-row-detail", "Create an empty directory on isolated-baseline" }
                    }
                }
                div { class: "dialog-footer",
                    span { "↑+↓" }
                    span { "Navigate" }
                    span { "⏎" }
                    span { "Select" }
                    span { "Esc" }
                    span { "Close" }
                }
            }
        }
    }
}

/// Produces deterministic HTML for DOM and accessibility-contract checks.
#[cfg(feature = "host")]
#[must_use]
pub fn render_shell_html() -> String {
    dioxus_ssr::render_element(rsx! { SpockyShell {} })
}

/// Produces the deterministic Add Project dialog outcome for host contract checks.
#[cfg(feature = "host")]
#[must_use]
pub fn render_add_project_dialog_html() -> String {
    dioxus_ssr::render_element(rsx! { AddProjectDialog {} })
}

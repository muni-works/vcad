import * as Dialog from "@radix-ui/react-dialog";
import { X } from "@phosphor-icons/react/dist/ssr/X";
import { Cube } from "@phosphor-icons/react/dist/ssr/Cube";
import { Sparkle } from "@phosphor-icons/react/dist/ssr/Sparkle";
import { FileArrowUp } from "@phosphor-icons/react/dist/ssr/FileArrowUp";
import { Globe } from "@phosphor-icons/react/dist/ssr/Globe";
import { GitBranch } from "@phosphor-icons/react/dist/ssr/GitBranch";
import { Lightning } from "@phosphor-icons/react/dist/ssr/Lightning";
import { GithubLogo } from "@phosphor-icons/react/dist/ssr/GithubLogo";
import { Book } from "@phosphor-icons/react/dist/ssr/Book";
import { ChatCircle } from "@phosphor-icons/react/dist/ssr/ChatCircle";
import { cn } from "@/lib/utils";

// ---------------------------------------------------------------------------
// Feature grid — the product pitch. Each card is one "why vcad" reason.
// ---------------------------------------------------------------------------

const FEATURES = [
  {
    icon: Cube,
    title: "Real BRep kernel",
    desc: "Solid modeling with half-edge topology, NURBS, and exact predicates — not mesh hacks.",
    color: "text-emerald-400",
  },
  {
    icon: Sparkle,
    title: "AI-native",
    desc: "Describe what you want in words. The assistant creates, modifies, and explains geometry.",
    color: "text-brand",
  },
  {
    icon: FileArrowUp,
    title: "STEP in, STL out",
    desc: "Drag-drop STEP import. Export to STL, GLB, STEP, DXF. Interops with Fusion, SolidWorks, etc.",
    color: "text-amber-400",
  },
  {
    icon: Globe,
    title: "Runs in your browser",
    desc: "No install, no GPU requirement. WebAssembly kernel + WebGL viewport. Works on any device.",
    color: "text-blue-400",
  },
  {
    icon: GitBranch,
    title: "Open source",
    desc: "Your data, your terms. Self-host or use vcad.io. MIT-licensed kernel, Apache-2 app.",
    color: "text-violet-400",
  },
  {
    icon: Lightning,
    title: "Physics & simulation",
    desc: "phyz articulated rigid-body physics, joint simulation, and gym-style RL interface for robotics.",
    color: "text-orange-400",
  },
] as const;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function ProductModal({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-black/50 backdrop-blur-sm" />
        <Dialog.Content
          className={cn(
            "fixed left-1/2 top-1/2 z-50 w-full max-w-lg -translate-x-1/2 -translate-y-1/2",
            "rounded-xl border border-border bg-surface shadow-xl select-none",
            "focus:outline-none",
            "max-h-[90vh] overflow-y-auto scrollbar-thin",
          )}
        >
          <Dialog.Close className="sticky top-0 float-right z-10 m-3 p-1.5 text-text-muted hover:text-text hover:bg-hover transition-colors cursor-pointer">
            <X size={14} />
          </Dialog.Close>

          <div className="p-8 pt-6">
            {/* Hero */}
            <div className="text-center mb-6">
              <Dialog.Title className="text-3xl font-bold tracking-tighter text-text mb-1">
                vcad<span className="text-brand">.</span>
              </Dialog.Title>
              <Dialog.Description className="text-xs text-text-muted">
                Open-source parametric CAD, in your browser
              </Dialog.Description>
            </div>

            {/* Feature grid */}
            <div className="grid grid-cols-2 gap-2 mb-8">
              {FEATURES.map((f) => (
                <div
                  key={f.title}
                  className="flex flex-col gap-1 p-3 bg-bg"
                >
                  <f.icon size={16} className={f.color} />
                  <span className="text-[11px] font-semibold text-text">
                    {f.title}
                  </span>
                  <span className="text-[10px] leading-snug text-text-muted">
                    {f.desc}
                  </span>
                </div>
              ))}
            </div>

            {/* Footer links */}
            <div className="flex items-center justify-center gap-6 text-xs">
              <a
                href="https://github.com/nicholaschuayunzhi/vcad"
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1.5 text-text-muted hover:text-text transition-colors"
              >
                <GithubLogo size={13} />
                <span>GitHub</span>
              </a>
              <a
                href="https://vcad.io/docs"
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1.5 text-text-muted hover:text-text transition-colors"
              >
                <Book size={13} />
                <span>Docs</span>
              </a>
              <a
                href="https://discord.gg/vcad"
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1.5 text-text-muted hover:text-text transition-colors"
              >
                <ChatCircle size={13} />
                <span>Discord</span>
              </a>
            </div>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

import { useDebouncedValue, useHealth } from "@aqora/pisa-react";
import { useEffect, useState } from "react";

import { HealthBadge } from "./components/HealthBadge.tsx";
import { inputClass } from "./components/ui.tsx";
import { CheckTab } from "./tabs/CheckTab.tsx";
import { ProveTab } from "./tabs/ProveTab.tsx";
import { SearchTab } from "./tabs/SearchTab.tsx";
import { SnapshotsTab } from "./tabs/SnapshotsTab.tsx";

const TABS = { check: "Check", prove: "Prove", snapshots: "Snapshots", search: "Search" } as const;
type Tab = keyof typeof TABS;

const CHECK_EXAMPLE = `theorem add_zero' (n : Nat) : n + 0 = n := by
  simp

theorem two_mul' (n : Nat) : 2 * n = n + n := by
  omega

#check Nat.add_comm`;

function tabOf(hash: string): Tab {
  const name = hash.replace(/^#/, "");
  return name in TABS ? (name as Tab) : "check";
}

/** The tab in the URL's hash. */
function useTab(): [Tab, (tab: Tab) => void] {
  const [tab, setTab] = useState(() => tabOf(window.location.hash));
  useEffect(() => {
    const changed = () => setTab(tabOf(window.location.hash));
    window.addEventListener("hashchange", changed);
    return () => window.removeEventListener("hashchange", changed);
  }, []);
  return [tab, (t) => (window.location.hash = t)];
}

export function App() {
  const health = useHealth();
  const [tab, setTab] = useTab();
  const [typed, setTyped] = useState("");
  // Sent once it settles, not as each letter is typed.
  const version = useDebouncedValue(typed, 500);
  const [checkCode, setCheckCode] = useState(CHECK_EXAMPLE);
  const searches = health.data?.search != null;
  const tabs = (Object.keys(TABS) as Tab[]).filter((t) => t !== "search" || searches);

  return (
    <div className="min-h-screen bg-slate-50 text-slate-900">
      <header className="border-b border-slate-200 bg-white">
        <div className="mx-auto flex max-w-7xl flex-wrap items-center gap-4 px-4 py-3">
          <h1 className="text-lg font-semibold">
            pisa <span className="font-normal text-slate-500">playground</span>
          </h1>
          <HealthBadge />
          <label className="ml-auto flex items-center gap-2 text-xs text-slate-600">
            version
            <input className={`${inputClass} w-48`} value={typed} onChange={(e) => setTyped(e.target.value.trim())} placeholder="the server's default" />
          </label>
        </div>
        <nav className="mx-auto flex max-w-7xl gap-1 px-4" role="tablist">
          {tabs.map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={t === tab}
              onClick={() => setTab(t)}
              className={`border-b-2 px-3 py-2 text-sm font-medium ${
                t === tab ? "border-indigo-600 text-indigo-700" : "border-transparent text-slate-600 hover:text-slate-900"
              }`}
            >
              {TABS[t]}
            </button>
          ))}
        </nav>
      </header>
      <main className="mx-auto max-w-7xl px-4 py-4">
        {/* Hidden tabs stay mounted, effects and all: a proof session goes
            on, and its requests aren't aborted, while its tab is hidden. */}
        <div role="tabpanel" hidden={tab !== "check"}>
          <CheckTab code={checkCode} setCode={setCheckCode} version={version} />
        </div>
        <div role="tabpanel" hidden={tab !== "prove"}>
          <ProveTab version={version} />
        </div>
        <div role="tabpanel" hidden={tab !== "snapshots"}>
          <SnapshotsTab
            version={version}
            onUse={(header) => {
              setCheckCode(`${header}\n\n`);
              setTab("check");
            }}
          />
        </div>
        <div role="tabpanel" hidden={!(tab === "search" && searches)}>
          <SearchTab version={version} />
        </div>
        {tab === "search" && !searches && <p className="text-sm text-slate-500">This server doesn't search.</p>}
      </main>
    </div>
  );
}

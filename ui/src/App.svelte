<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "./lib/daemon.svelte";
  import DeviceCard from "./lib/DeviceCard.svelte";
  import GamesPanel from "./lib/GamesPanel.svelte";
  import HeadsetPanel from "./lib/HeadsetPanel.svelte";
  import KeyboardPanel from "./lib/KeyboardPanel.svelte";
  import MousePanel from "./lib/MousePanel.svelte";
  import SonarPanel from "./lib/SonarPanel.svelte";
  import Icon from "./lib/Icon.svelte";
  import type { IconName } from "./lib/icons";

  type Page = "engine" | "sonar" | "games" | "moments" | "settings";

  const NAV: { id: Page; icon: IconName; label: string; blurb: string }[] = [
    { id: "engine", icon: "engine", label: "Engine", blurb: "Devices, lighting, bindings" },
    { id: "sonar", icon: "sonar", label: "Sonar", blurb: "Per-app audio routing and EQ" },
    { id: "games", icon: "games", label: "Games", blurb: "GameSense: games drive your lighting" },
    { id: "moments", icon: "moments", label: "Moments", blurb: "Instant replay and clips" },
    { id: "settings", icon: "settings", label: "Settings", blurb: "Profiles, updates" },
  ];

  // Milestones that will fill the not-yet-built pages (see PLAN.md).
  const COMING: Record<Exclude<Page, "engine" | "sonar" | "games">, string> = {
    moments: "M7 — gpu-screen-recorder replay buffer and clip library",
    settings: "M7 — profiles and per-game switching",
  };

  let page = $state<Page>("engine");

  /** Devices with a control panel so far. */
  const RIVAL3_PID = 0x1872;
  const NOVA_PRO_PID = 0x12e0;
  const APEX_PID = 0x1642;
  let hasRival3 = $derived(daemon.devices.some((d) => d.product_id === RIVAL3_PID));
  let hasNovaPro = $derived(daemon.devices.some((d) => d.product_id === NOVA_PRO_PID));
  let hasApex = $derived(daemon.devices.some((d) => d.product_id === APEX_PID));

  onMount(() => {
    void daemon.start();
  });
</script>

<div class="shell">
  <nav>
    <div class="brand">
      <span class="mark"></span>
      <span class="wordmark">gglinux</span>
    </div>

    {#each NAV as item (item.id)}
      <button class="nav-item" class:active={page === item.id} onclick={() => (page = item.id)}>
        <Icon name={item.icon} />
        <span>{item.label}</span>
      </button>
    {/each}

    <div class="spacer"></div>

    <div class="status" class:online={daemon.connected}>
      <span class="pip"></span>
      {daemon.connected ? "daemon connected" : "daemon offline"}
    </div>
  </nav>

  <main>
    {#if page === "engine"}
      <header class="page-head">
        <h1>Engine</h1>
        <p>{NAV[0].blurb}</p>
      </header>

      {#if !daemon.connected}
        <section class="notice">
          <h2>gglinuxd isn&rsquo;t running</h2>
          <p>Start the daemon, then this list populates automatically:</p>
          <pre><code>systemctl --user enable --now gglinuxd</code></pre>
          <p class="fine">The UI reconnects on its own — no need to restart it.</p>
        </section>
      {:else if daemon.devices.length === 0}
        <section class="notice">
          <h2>No SteelSeries devices found</h2>
          <p>
            Connected devices show up here the moment they&rsquo;re plugged in. If yours is
            attached but missing, the udev rule may not be installed:
          </p>
          <pre><code>sudo install -Dm644 data/udev/60-gglinux.rules \
  /etc/udev/rules.d/60-gglinux.rules
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=hidraw --action=add</code></pre>
        </section>
      {:else}
        {#if hasRival3}
          <section class="controls">
            <MousePanel />
          </section>
        {/if}

        {#if hasNovaPro}
          <section class="controls">
            <HeadsetPanel />
          </section>
        {/if}

        {#if hasApex}
          <section class="controls">
            <KeyboardPanel />
          </section>
        {/if}

        <div class="grid">
          {#each daemon.devices as device (device.id)}
            <DeviceCard {device} />
          {/each}
        </div>
      {/if}

      {#if daemon.lastError}
        <p class="error">{daemon.lastError}</p>
      {/if}
    {:else if page === "sonar"}
      <header class="page-head">
        <h1>Sonar</h1>
        <p>{NAV[1].blurb}</p>
      </header>
      {#if daemon.connected}
        <SonarPanel />
      {:else}
        <section class="notice"><h2>gglinuxd isn&rsquo;t running</h2></section>
      {/if}
    {:else if page === "games"}
      <header class="page-head">
        <h1>Games</h1>
        <p>{NAV[2].blurb}</p>
      </header>
      {#if daemon.connected}
        <GamesPanel />
      {:else}
        <section class="notice"><h2>gglinuxd isn&rsquo;t running</h2></section>
      {/if}
    {:else}
      {@const item = NAV.find((n) => n.id === page)!}
      <header class="page-head">
        <h1>{item.label}</h1>
        <p>{item.blurb}</p>
      </header>
      <section class="notice">
        <h2>Not built yet</h2>
        <p>{COMING[page]}</p>
      </section>
    {/if}
  </main>
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 218px 1fr;
    height: 100vh;
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    padding: 1rem 0.65rem;
    background: var(--surface);
    border-right: 1px solid var(--line);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.25rem 0.5rem 1.1rem;
  }
  .mark {
    width: 20px;
    height: 20px;
    border-radius: 6px;
    background: linear-gradient(140deg, var(--accent), var(--accent-2));
  }
  .wordmark {
    font-weight: 650;
    letter-spacing: -0.01em;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 0.55rem;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--text-dim);
    font: inherit;
    font-size: 0.88rem;
    text-align: left;
    cursor: pointer;
  }
  .nav-item:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .nav-item.active {
    background: var(--surface-2);
    color: var(--text);
    box-shadow: inset 2px 0 0 var(--accent);
  }

  .spacer {
    flex: 1;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.5rem 0.55rem;
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .pip {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-dim);
  }
  .status.online .pip {
    background: var(--ok);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 20%, transparent);
  }

  main {
    overflow-y: auto;
    padding: 1.6rem 1.8rem 2.5rem;
  }
  .page-head {
    margin-bottom: 1.3rem;
  }
  .page-head h1 {
    margin: 0;
    font-size: 1.35rem;
    font-weight: 620;
    letter-spacing: -0.01em;
  }
  .page-head p {
    margin: 0.25rem 0 0;
    color: var(--text-dim);
    font-size: 0.86rem;
  }

  .grid {
    display: grid;
    gap: 0.85rem;
    grid-template-columns: repeat(auto-fit, minmax(360px, 1fr));
  }

  /* Capped: the controls are a column of short rows, and stretching them across a
     wide window leaves each label marooned from its input. */
  .controls {
    max-width: 560px;
    margin-bottom: 1.1rem;
  }

  .notice {
    max-width: 620px;
    padding: 1.2rem 1.3rem;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: 12px;
  }
  .notice h2 {
    margin: 0 0 0.4rem;
    font-size: 1rem;
    font-weight: 600;
  }
  .notice p {
    margin: 0 0 0.7rem;
    color: var(--text-dim);
    font-size: 0.86rem;
    line-height: 1.5;
  }
  .notice pre {
    margin: 0;
    padding: 0.7rem 0.85rem;
    overflow-x: auto;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    font-size: 0.78rem;
  }
  .notice code {
    font-family: var(--mono);
    color: var(--accent);
  }
  .fine {
    margin: 0.7rem 0 0;
    font-size: 0.78rem;
  }
  .error {
    margin-top: 1rem;
    color: var(--danger);
    font-size: 0.82rem;
  }
</style>

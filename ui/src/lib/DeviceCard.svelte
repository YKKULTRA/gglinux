<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { DeviceInfo, InterfaceInfo } from "./types";
  import type { IconName } from "./icons";

  let { device }: { device: DeviceInfo } = $props();

  const hex4 = (n: number) => n.toString(16).padStart(4, "0");

  /** The vendor-defined HID usage pages carry the control protocols we care about. */
  const isVendorPage = (i: InterfaceInfo) => i.usage_page >= 0xff00;

  let controlIfaces = $derived(device.interfaces.filter(isVendorPage));
</script>

<article class="card">
  <div class="glyph"><Icon name={device.kind as IconName} size={26} /></div>

  <div class="body">
    <header>
      <h3>{device.name}</h3>
      <span class="badge" class:online={device.connected}>
        {device.connected ? "connected" : "offline"}
      </span>
    </header>

    <p class="meta">
      <code>{hex4(device.vendor_id)}:{hex4(device.product_id)}</code>
      <span class="dot">·</span>
      {device.kind}
      {#if device.serial}
        <span class="dot">·</span> sn {device.serial}
      {/if}
    </p>

    <details>
      <summary>{device.interfaces.length} HID interfaces &middot; {controlIfaces.length} vendor</summary>
      <table>
        <thead>
          <tr><th>iface</th><th>usage page</th><th>usage</th><th>node</th></tr>
        </thead>
        <tbody>
          {#each device.interfaces as i (i.path + i.usage_page + i.usage)}
            <tr class:vendor={isVendorPage(i)}>
              <td>{i.interface_number}</td>
              <td>0x{hex4(i.usage_page)}</td>
              <td>0x{i.usage.toString(16).padStart(2, "0")}</td>
              <td class="path">{i.path}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </details>
  </div>
</article>

<style>
  .card {
    display: flex;
    gap: 1rem;
    padding: 1rem 1.1rem;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: 12px;
  }
  .glyph {
    flex: none;
    width: 46px;
    height: 46px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    background: var(--surface-2);
    color: var(--accent);
  }
  .body {
    min-width: 0;
    flex: 1;
  }
  header {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  h3 {
    margin: 0;
    font-size: 0.98rem;
    font-weight: 600;
  }
  .badge {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0.12rem 0.45rem;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--text-dim);
  }
  .badge.online {
    background: color-mix(in srgb, var(--ok) 18%, transparent);
    color: var(--ok);
  }
  .meta {
    margin: 0.3rem 0 0.55rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .dot {
    opacity: 0.5;
    margin: 0 0.2rem;
  }
  summary {
    cursor: pointer;
    font-size: 0.8rem;
    color: var(--text-dim);
  }
  summary:hover {
    color: var(--text);
  }
  table {
    width: 100%;
    margin-top: 0.6rem;
    border-collapse: collapse;
    font-size: 0.76rem;
  }
  th {
    text-align: left;
    font-weight: 500;
    color: var(--text-dim);
    padding-bottom: 0.25rem;
  }
  td {
    padding: 0.15rem 0.5rem 0.15rem 0;
    color: var(--text-dim);
  }
  tr.vendor td {
    color: var(--accent);
  }
  .path {
    font-family: var(--mono);
  }
  code {
    font-family: var(--mono);
  }
</style>

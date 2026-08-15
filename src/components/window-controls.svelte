<script lang="ts">
    import { chrome } from "$lib/chrome.svelte";

    /**
     * Minimise, maximise and close.
     *
     * The glyphs are hand-drawn rather than taken from the icon set. Window
     * controls are the one place where matching the platform matters more than
     * matching the app: these are 10px square, crosshair-thin, and sit at the
     * exact size Windows draws them, because a rounded lucide `x` at this size
     * reads as a button someone forgot to style.
     *
     * Sizing is fixed in pixels for the same reason -- these have to line up
     * with the physical corner of the screen when maximised, and a rem-based
     * size would drift with the font scale.
     */

    /** Close is the only one that gets a colour, and only on hover. */
    const base =
        "inline-grid h-8 w-[46px] place-items-center text-titlebar-foreground transition-colors duration-75 hover:bg-foreground/10 active:bg-foreground/[0.16] focus-visible:outline-none focus-visible:bg-foreground/10";
</script>

<!--
  No drag region here: a drag started on a button would move the window instead
  of pressing it. The gap is why the titlebar marks its regions individually
  rather than wrapping everything.
-->
<div class="flex shrink-0 items-stretch">
    <button
        type="button"
        class={base}
        aria-label="Minimise"
        title="Minimise"
        onclick={() => chrome.minimize()}
    >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path d="M0 5h10" stroke="currentColor" stroke-width="1" />
        </svg>
    </button>

    <button
        type="button"
        class={base}
        aria-label={chrome.maximized ? "Restore" : "Maximise"}
        title={chrome.maximized ? "Restore" : "Maximise"}
        onclick={() => chrome.toggleMaximize()}
    >
        {#if chrome.maximized}
            <!-- Two offset squares: the standard "restore down" glyph. -->
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <path
                    d="M2.5 2.5V0.5h7v7h-2"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1"
                />
                <rect
                    x="0.5"
                    y="2.5"
                    width="7"
                    height="7"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1"
                />
            </svg>
        {:else}
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <rect
                    x="0.5"
                    y="0.5"
                    width="9"
                    height="9"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1"
                />
            </svg>
        {/if}
    </button>

    <button
        type="button"
        class="{base} hover:!bg-destructive hover:!text-destructive-foreground active:!bg-destructive/85"
        aria-label="Close"
        title="Close"
        onclick={() => chrome.close()}
    >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" stroke-width="1" />
        </svg>
    </button>
</div>

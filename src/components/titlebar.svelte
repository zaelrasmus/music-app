<script lang="ts">
    import WindowControls from "$components/window-controls.svelte";
    import ThemeToggle from "$components/theme-toggle.svelte";
    import ActivityButton from "$components/activity-button.svelte";
    import { sidebar } from "$lib/sidebar.svelte";
    import PanelLeftIcon from "@lucide/svelte/icons/panel-left";
    import PanelLeftCloseIcon from "@lucide/svelte/icons/panel-left-close";
    import PanelLeftDashedIcon from "@lucide/svelte/icons/panel-left-dashed";

    /**
     * The window frame.
     *
     * Almost empty, which is the demo's decision and the right one. The bar
     * used to carry the view's name and the playing track; both were already
     * on screen -- the view name above the list it names, the track in the
     * player bar -- so the titlebar was repeating things rather than saying
     * anything. An empty strip reads as part of the window; a busy one reads
     * as a toolbar you have to check.
     *
     * `data-tauri-drag-region` is what makes an undecorated window movable, and
     * it acts on the element the press lands on -- so it goes on the bar and on
     * the inert middle, never on a control. Tauri also handles double-click to
     * maximise on those same regions, which is the gesture people reach for
     * without being told.
     */

    const TOGGLE_LABELS = {
        expanded: "Collapse sidebar to icons",
        icons: "Hide sidebar",
        hidden: "Show sidebar",
    } as const;

    const toggleLabel = $derived(TOGGLE_LABELS[sidebar.mode]);
</script>

<header
    data-tauri-drag-region
    class="bg-titlebar flex h-9 shrink-0 items-center gap-1 pl-1.5"
>
    <!--
      The one control that has to live here. Hiding the sidebar removes every
      other way to bring it back, so the toggle cannot live inside the thing it
      hides.
    -->
    <button
        type="button"
        class="text-titlebar-foreground hover:bg-foreground/10 hover:text-foreground focus-visible:bg-foreground/10 inline-grid size-7 shrink-0 place-items-center rounded-md transition-colors focus-visible:outline-none"
        aria-label={toggleLabel}
        title="{toggleLabel}  (Ctrl+B)"
        onclick={() => sidebar.cycle()}
    >
        {#if sidebar.mode === "expanded"}
            <PanelLeftCloseIcon class="size-[15px]" />
        {:else if sidebar.mode === "icons"}
            <PanelLeftDashedIcon class="size-[15px]" />
        {:else}
            <PanelLeftIcon class="size-[15px]" />
        {/if}
    </button>

    <div data-tauri-drag-region class="h-full flex-1"></div>

    <div class="flex shrink-0 items-center gap-0.5 pr-1">
        <!-- Only present while there is something to report. -->
        <ActivityButton />
        <ThemeToggle chrome />
    </div>

    <WindowControls />
</header>

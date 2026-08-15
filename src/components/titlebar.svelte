<script lang="ts">
    import WindowControls from "$components/window-controls.svelte";
    import ThemeToggle from "$components/theme-toggle.svelte";
    import { sidebar } from "$lib/sidebar.svelte";
    import { nav } from "$lib/nav.svelte";
    import { player } from "$lib/player.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import PanelLeftIcon from "@lucide/svelte/icons/panel-left";
    import PanelLeftCloseIcon from "@lucide/svelte/icons/panel-left-close";
    import PanelLeftDashedIcon from "@lucide/svelte/icons/panel-left-dashed";

    /**
     * The window frame.
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

    /**
     * The title shows what is playing, not the app's name.
     *
     * The app's name is on the taskbar already, and this is the one strip of
     * the window that is visible whichever view is open -- so it is worth
     * spending on the only fact that outlives navigation.
     */
    const nowPlaying = $derived(queueStore.current);

    const VIEW_NAMES = {
        library: "Library",
        search: "Search",
        playlists: "Playlists",
        history: "Recently played",
        settings: "Settings",
    } as const;
</script>

<header
    data-tauri-drag-region
    class="bg-titlebar border-border/70 flex h-10 shrink-0 items-center gap-1 border-b pl-1.5"
>
    <button
        type="button"
        class="text-titlebar-foreground hover:bg-foreground/10 hover:text-foreground focus-visible:bg-foreground/10 inline-grid size-8 shrink-0 place-items-center rounded-md transition-colors focus-visible:outline-none"
        aria-label={toggleLabel}
        title="{toggleLabel}  (Ctrl+B)"
        onclick={() => sidebar.cycle()}
    >
        {#if sidebar.mode === "expanded"}
            <PanelLeftCloseIcon class="size-4" />
        {:else if sidebar.mode === "icons"}
            <PanelLeftDashedIcon class="size-4" />
        {:else}
            <PanelLeftIcon class="size-4" />
        {/if}
    </button>

    <span
        class="text-titlebar-foreground shrink-0 px-1 text-xs font-medium"
        data-tauri-drag-region
    >
        {VIEW_NAMES[nav.view]}
    </span>

    <!-- The draggable middle. Also where the window title lives, so the bar
         still says something when nothing is playing. -->
    <div
        data-tauri-drag-region
        class="flex min-w-0 flex-1 items-center justify-center gap-1.5 px-2"
    >
        {#if nowPlaying}
            <span
                data-tauri-drag-region
                class="text-titlebar-foreground min-w-0 truncate text-xs"
            >
                <span class="text-foreground/80 font-medium">{nowPlaying.title}</span>
                <span class="opacity-70"> — {nowPlaying.artist ?? "Unknown artist"}</span>
            </span>
            {#if player.stalled}
                <span
                    data-tauri-drag-region
                    class="text-titlebar-foreground shrink-0 text-[11px] opacity-80"
                >
                    · reconnecting
                </span>
            {/if}
        {:else}
            <span data-tauri-drag-region class="text-titlebar-foreground text-xs opacity-60">
                music-app
            </span>
        {/if}
    </div>

    <div class="flex shrink-0 items-center gap-0.5 pr-1">
        <ThemeToggle chrome />
    </div>

    <WindowControls />
</header>

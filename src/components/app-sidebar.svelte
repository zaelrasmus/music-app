<script lang="ts">
    import { nav, type View } from "$lib/nav.svelte";
    import {
        sidebar,
        MIN_WIDTH,
        MAX_WIDTH,
        ICON_WIDTH,
    } from "$lib/sidebar.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { cacheStore, formatBytes } from "$lib/cache.svelte";
    import { libraryView } from "$lib/library-view.svelte";
    import { historyStore } from "$lib/history.svelte";
    import LibraryIcon from "@lucide/svelte/icons/library";
    import SearchIcon from "@lucide/svelte/icons/search";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import HistoryIcon from "@lucide/svelte/icons/history";
    import SettingsIcon from "@lucide/svelte/icons/settings";
    import HardDriveIcon from "@lucide/svelte/icons/hard-drive";
    import type { Component } from "svelte";

    /**
     * The sidebar, built by hand.
     *
     * Three modes with one rule between them: nothing is *reachable only* in
     * expanded mode. The rail keeps every destination, and the sections it
     * cannot show (the playlist shortcuts, the storage meter) are shortcuts to
     * places the rail still links to. Collapsing therefore costs reach, never
     * capability -- which is what makes hiding it entirely a safe thing to
     * offer.
     */

    type Item = {
        view: View;
        label: string;
        icon: Component;
        /** A quiet number on the right; absent rather than zero when empty. */
        count?: number;
    };

    const items = $derived<Item[]>([
        {
            view: "library",
            label: "Library",
            icon: LibraryIcon,
            count: libraryView.results.length || undefined,
        },
        { view: "search", label: "Search", icon: SearchIcon },
        {
            view: "playlists",
            label: "Playlists",
            icon: ListMusicIcon,
            count: playlistStore.playlists.length || undefined,
        },
        {
            view: "history",
            label: "Recently played",
            icon: HistoryIcon,
            count: historyStore.tracks.length || undefined,
        },
        { view: "settings", label: "Settings", icon: SettingsIcon },
    ]);

    const expanded = $derived(sidebar.mode === "expanded");
    const hidden = $derived(sidebar.mode === "hidden");

    /** Only the first few; the Playlists view is one click away for the rest. */
    const shortcuts = $derived(playlistStore.playlists.slice(0, 7));

    const usedFraction = $derived(
        cacheStore.limitBytes === 0
            ? 0
            : Math.min(cacheStore.usedBytes / cacheStore.limitBytes, 1),
    );

    function open(playlistId: number) {
        nav.go("playlists");
        void playlistStore.openPlaylist(playlistId);
    }

    // --- Resizing -----------------------------------------------------------

    let handle = $state<HTMLDivElement | null>(null);

    function startResize(event: PointerEvent) {
        // Pointer capture, not a window listener: the pointer routinely leaves
        // the 6px handle within the first frame of a drag, and capture is what
        // keeps the events coming to us instead of to whatever is underneath.
        handle?.setPointerCapture(event.pointerId);
        sidebar.resizing = true;
        event.preventDefault();
    }

    function moveResize(event: PointerEvent) {
        if (!sidebar.resizing) return;
        // The sidebar is flush against the window's left edge, so the pointer's
        // x is the width. No measuring needed, and nothing to go stale.
        sidebar.drag(event.clientX);
    }

    function endResize(event: PointerEvent) {
        if (!sidebar.resizing) return;
        handle?.releasePointerCapture(event.pointerId);
        sidebar.commit();
    }

    /** Keyboard resizing, because a 6px drag target is not an accessible one. */
    function keyResize(event: KeyboardEvent) {
        const step = event.shiftKey ? 32 : 8;
        if (event.key === "ArrowLeft") {
            sidebar.drag(sidebar.effectiveWidth - step);
        } else if (event.key === "ArrowRight") {
            sidebar.drag(sidebar.effectiveWidth + step);
        } else if (event.key === "Home") {
            sidebar.reset();
            return;
        } else {
            return;
        }
        event.preventDefault();
        sidebar.commit();
    }
</script>

{#snippet navButton(item: Item)}
    {@const active = nav.view === item.view}
    <button
        type="button"
        class="group/item relative flex h-9 w-full items-center rounded-md text-sm transition-colors
               {expanded ? 'gap-2.5 px-2.5' : 'justify-center px-0'}
               {active
            ? 'bg-sidebar-accent text-sidebar-accent-foreground font-medium'
            : 'text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground'}"
        aria-current={active ? "page" : undefined}
        aria-label={item.label}
        onclick={() => nav.go(item.view)}
    >
        <!-- The active marker is a bar on the sidebar's own edge rather than a
             filled row: it survives being 60px wide, so the rail and the
             expanded sidebar indicate the current view the same way. -->
        {#if active}
            <span
                class="bg-primary absolute top-1/2 -left-2 h-4 w-[3px] -translate-y-1/2 rounded-full"
            ></span>
        {/if}

        <item.icon class="size-[18px] shrink-0" />

        {#if expanded}
            <span class="min-w-0 flex-1 truncate text-left">{item.label}</span>
            {#if item.count !== undefined}
                <span class="text-muted-foreground shrink-0 text-[11px] tabular-nums">
                    {item.count}
                </span>
            {/if}
        {:else}
            <!-- The rail's label. A real element rather than `title`, which
                 waits a second and then renders in the OS's font. -->
            <span
                class="bg-popover text-popover-foreground pointer-events-none absolute top-1/2 left-[calc(100%+10px)] z-50 -translate-y-1/2 rounded-md border px-2 py-1 text-xs whitespace-nowrap opacity-0 shadow-md transition-opacity duration-100 group-hover/item:opacity-100"
                role="tooltip"
            >
                {item.label}
            </span>
        {/if}
    </button>
{/snippet}

<aside
    class="bg-sidebar relative z-10 flex shrink-0 flex-col overflow-hidden
           {sidebar.resizing ? '' : 'transition-[width] duration-200 ease-out'}
           {hidden ? '' : 'border-border/70 border-r'}"
    style="width: {sidebar.effectiveWidth}px"
    aria-label="Main"
    aria-hidden={hidden}
    inert={hidden}
>
    <!-- Held at its natural width while the container animates, so the labels
         slide out of view instead of reflowing word by word. -->
    <div
        class="flex min-h-0 flex-1 flex-col"
        style="width: {expanded ? `${sidebar.width}px` : `${ICON_WIDTH}px`}"
    >
        <nav class="flex flex-col gap-0.5 px-2 pt-3">
            {#each items as item (item.view)}
                {@render navButton(item)}
            {/each}
        </nav>

        {#if expanded}
            <div class="seam mx-3 mt-3 h-px shrink-0"></div>

            <div class="min-h-0 flex-1 overflow-y-auto px-2 py-2">
                <h2
                    class="text-muted-foreground px-2.5 pb-1 text-[10px] font-semibold tracking-[0.08em] uppercase"
                >
                    Playlists
                </h2>

                {#if shortcuts.length === 0}
                    <p class="text-muted-foreground/80 px-2.5 py-1 text-xs">
                        None yet.
                    </p>
                {:else}
                    <ul class="flex flex-col gap-px">
                        {#each shortcuts as playlist (playlist.id)}
                            {@const active =
                                nav.view === "playlists" &&
                                playlistStore.open?.playlist.id === playlist.id}
                            <li>
                                <button
                                    type="button"
                                    class="hover:bg-sidebar-accent/60 flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[13px] transition-colors
                                           {active
                                        ? 'text-sidebar-accent-foreground font-medium'
                                        : 'text-sidebar-foreground/70 hover:text-sidebar-foreground'}"
                                    onclick={() => open(playlist.id)}
                                >
                                    <span class="min-w-0 flex-1 truncate">
                                        {playlist.name}
                                    </span>
                                    <span
                                        class="text-muted-foreground shrink-0 text-[11px] tabular-nums"
                                    >
                                        {playlist.trackCount}
                                    </span>
                                </button>
                            </li>
                        {/each}
                    </ul>

                    {#if playlistStore.playlists.length > shortcuts.length}
                        <button
                            type="button"
                            class="text-muted-foreground hover:text-foreground w-full px-2.5 py-1 text-left text-[11px] transition-colors"
                            onclick={() => nav.go("playlists")}
                        >
                            {playlistStore.playlists.length - shortcuts.length} more…
                        </button>
                    {/if}
                {/if}
            </div>

            <!-- Storage lives here because it is the one number that governs
                 whether the offline badges elsewhere keep their promise. -->
            <button
                type="button"
                class="hover:bg-sidebar-accent/60 border-border/70 m-2 flex flex-col gap-1.5 rounded-md border border-transparent px-2.5 py-2 text-left transition-colors"
                onclick={() => nav.go("settings")}
                title="Streaming cache — {formatBytes(
                    cacheStore.usedBytes,
                )} of {formatBytes(cacheStore.limitBytes)}"
            >
                <span
                    class="text-muted-foreground flex items-center gap-1.5 text-[11px]"
                >
                    <HardDriveIcon class="size-3" />
                    <span class="flex-1 truncate">Cache</span>
                    <span class="tabular-nums">
                        {formatBytes(cacheStore.usedBytes)}
                    </span>
                </span>
                <span class="bg-muted h-1 overflow-hidden rounded-full">
                    <span
                        class="bg-primary/70 block h-full rounded-full transition-[width] duration-300"
                        style="width: {usedFraction * 100}%"
                    ></span>
                </span>
            </button>
        {:else}
            <div class="flex-1"></div>
        {/if}
    </div>

    {#if !hidden}
        <!--
          The resize edge. Invisible until touched, and wider than the line it
          appears to be so it can actually be grabbed.

          Present in icons mode too, and not only for symmetry: a drag that
          collapses the sidebar would otherwise unmount its own handle
          mid-gesture, losing pointer capture and stranding `resizing` at true.
          Keeping it mounted means one continuous drag can collapse and expand
          again.
        -->
        <!--
          The linter treats `separator` as non-interactive, which is true of the
          decorative kind. This is the other kind: ARIA's window splitter, a
          focusable separator with valuenow/valuemin/valuemax that is *required*
          to be operable by keyboard. Both warnings describe the pattern being
          implemented correctly.
        -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div
            bind:this={handle}
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize sidebar"
            aria-valuenow={sidebar.effectiveWidth}
            aria-valuemin={MIN_WIDTH}
            aria-valuemax={MAX_WIDTH}
            tabindex="0"
            class="group/handle absolute inset-y-0 right-0 w-1.5 cursor-col-resize touch-none focus-visible:outline-none"
            onpointerdown={startResize}
            onpointermove={moveResize}
            onpointerup={endResize}
            onpointercancel={endResize}
            onkeydown={keyResize}
            ondblclick={() => sidebar.reset()}
        >
            <span
                class="bg-primary absolute inset-y-0 right-0 w-[2px] opacity-0 transition-opacity group-hover/handle:opacity-60 group-focus-visible/handle:opacity-100
                       {sidebar.resizing ? '!opacity-100' : ''}"
            ></span>
        </div>
    {/if}
</aside>

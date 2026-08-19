<script lang="ts">
    import { Button } from "$components/ui/button";
    import PageShell from "$components/page-shell.svelte";
    import SettingsSection from "$components/settings-section.svelte";
    import CacheSettings from "$components/cache-settings.svelte";
    import ExtractorSettings from "$components/extractor-settings.svelte";
    import TagManager from "$components/tag-manager.svelte";
    import { library } from "$lib/library.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import { sidebar, type SidebarMode } from "$lib/sidebar.svelte";
    import { mode, userPrefersMode, setMode, resetMode } from "mode-watcher";
    import FolderIcon from "@lucide/svelte/icons/folder";
    import FolderPlusIcon from "@lucide/svelte/icons/folder-plus";
    import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import PaletteIcon from "@lucide/svelte/icons/palette";
    import SunIcon from "@lucide/svelte/icons/sun";
    import MoonIcon from "@lucide/svelte/icons/moon";
    import MonitorIcon from "@lucide/svelte/icons/monitor";
    import PanelLeftIcon from "@lucide/svelte/icons/panel-left";
    import PanelLeftDashedIcon from "@lucide/svelte/icons/panel-left-dashed";
    import EyeOffIcon from "@lucide/svelte/icons/eye-off";

    const summary = $derived(trackStore.lastSummary);

    function formatDate(unixSeconds: number) {
        return new Date(unixSeconds * 1000).toLocaleDateString();
    }

    const THEMES = [
        { id: "light", label: "Light", icon: SunIcon },
        { id: "dark", label: "Dark", icon: MoonIcon },
        { id: "system", label: "System", icon: MonitorIcon },
    ] as const;

    const SIDEBARS: { id: SidebarMode; label: string; icon: typeof PanelLeftIcon }[] = [
        { id: "expanded", label: "Expanded", icon: PanelLeftIcon },
        { id: "icons", label: "Icons only", icon: PanelLeftDashedIcon },
        { id: "hidden", label: "Hidden", icon: EyeOffIcon },
    ];
</script>

<PageShell title="Settings" subtitle="Everything here is local to this machine.">
    <div class="mx-auto flex max-w-3xl flex-col gap-4 px-2">
        <SettingsSection
            icon={FolderIcon}
            title="Library folders"
            description="Folders scanned for music. Files stay where they are — nothing is copied or moved."
        >
            {#snippet actions()}
                <Button
                    variant="outline"
                    size="sm"
                    disabled={trackStore.scanning}
                    onclick={() => trackStore.rescan()}
                >
                    <RefreshCwIcon
                        data-icon="inline-start"
                        class={trackStore.scanning ? "animate-spin" : ""}
                    />
                    {trackStore.scanning ? "Scanning…" : "Rescan"}
                </Button>
                <Button
                    size="sm"
                    disabled={trackStore.scanning}
                    onclick={() => library.addFromPicker()}
                >
                    <FolderPlusIcon data-icon="inline-start" />
                    Add folder
                </Button>
            {/snippet}

            {#if library.error || trackStore.error}
                <p
                    class="border-destructive/50 bg-destructive/5 text-destructive selectable mb-3 rounded-md border px-3 py-2 text-sm"
                    role="alert"
                >
                    {library.error ?? trackStore.error}
                </p>
            {/if}

            {#if library.folders.length === 0}
                <p class="text-muted-foreground text-[13px]">
                    No folders yet. Add one and everything readable inside it
                    joins your library.
                </p>
            {:else}
                <ul class="flex flex-col gap-1">
                    {#each library.folders as folder (folder.id)}
                        <li
                            class="hover:bg-accent/50 flex items-center justify-between gap-3 rounded-md px-2 py-1.5 transition-colors"
                        >
                            <div class="flex min-w-0 flex-col">
                                <span class="selectable truncate text-[13px]">
                                    {folder.path}
                                </span>
                                <span class="text-muted-foreground text-xs">
                                    Added {formatDate(folder.addedAt)}
                                </span>
                            </div>
                            <button
                                type="button"
                                class="text-muted-foreground hover:bg-accent hover:text-destructive grid size-7 shrink-0 place-items-center rounded-md transition-colors disabled:opacity-40"
                                aria-label="Remove {folder.path}"
                                title="Stop scanning this folder"
                                disabled={trackStore.scanning}
                                onclick={() => library.remove(folder.id)}
                            >
                                <Trash2Icon class="size-4" />
                            </button>
                        </li>
                    {/each}
                </ul>
            {/if}

            {#if summary}
                <p class="text-muted-foreground mt-3 text-xs">
                    Last scan: {summary.scanned} seen · {summary.added} added · {summary.updated}
                    updated · {summary.unchanged} unchanged · {summary.markedMissing}
                    missing{#if summary.errors > 0}
                        · {summary.errors} unreadable{/if}{#if summary.skippedFolders.length > 0}
                        · {summary.skippedFolders.length} folder(s) unreachable{/if}
                </p>
            {/if}
        </SettingsSection>

        <SettingsSection
            icon={PaletteIcon}
            title="Appearance"
            description="Theme follows the system unless you pin it."
        >
            <div class="flex flex-col gap-4">
                <div class="flex flex-col gap-2">
                    <span class="text-muted-foreground text-xs font-medium">Theme</span>
                    <div class="flex flex-wrap gap-2">
                        {#each THEMES as choice (choice.id)}
                            {@const selected = userPrefersMode.current === choice.id}
                            <button
                                type="button"
                                class="flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px] transition-colors
                                       {selected
                                    ? 'border-primary bg-primary/10 text-foreground font-medium'
                                    : 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
                                aria-pressed={selected}
                                onclick={() =>
                                    choice.id === "system"
                                        ? resetMode()
                                        : setMode(choice.id)}
                            >
                                <choice.icon class="size-4" />
                                {choice.label}
                                {#if choice.id === "system"}
                                    <span class="text-muted-foreground text-xs">
                                        ({mode.current ?? "light"})
                                    </span>
                                {/if}
                            </button>
                        {/each}
                    </div>
                </div>

                <div class="flex flex-col gap-2">
                    <span class="text-muted-foreground text-xs font-medium">
                        Sidebar
                        <span class="opacity-70">— Ctrl+B cycles it, and the edge drags</span>
                    </span>
                    <div class="flex flex-wrap gap-2">
                        {#each SIDEBARS as choice (choice.id)}
                            {@const selected = sidebar.mode === choice.id}
                            <button
                                type="button"
                                class="flex items-center gap-2 rounded-lg border px-3 py-2 text-[13px] transition-colors
                                       {selected
                                    ? 'border-primary bg-primary/10 text-foreground font-medium'
                                    : 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
                                aria-pressed={selected}
                                onclick={() => sidebar.setMode(choice.id)}
                            >
                                <choice.icon class="size-4" />
                                {choice.label}
                            </button>
                        {/each}
                    </div>
                </div>
            </div>
        </SettingsSection>

        <CacheSettings />

        <ExtractorSettings />

        <TagManager />
    </div>
</PageShell>

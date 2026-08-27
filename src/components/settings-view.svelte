<script lang="ts">
    import { Button } from "$components/ui/button";
    import PageShell from "$components/page-shell.svelte";
    import SettingsSection from "$components/settings-section.svelte";
    import CacheSettings from "$components/cache-settings.svelte";
    import LoudnessSettings from "$components/loudness-settings.svelte";
    import EqualizerSettings from "$components/equalizer-settings.svelte";
    import GaplessSettings from "$components/gapless-settings.svelte";
    import SilenceSettings from "$components/silence-settings.svelte";
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
    import TriangleAlertIcon from "@lucide/svelte/icons/triangle-alert";
    import { decoder } from "$lib/decoder.svelte";

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

    /**
     * A number that moves, rather than a spinner that might mean anything.
     *
     * A thousand files takes long enough that "Scanning…" alone is
     * indistinguishable from a hang -- which is exactly how it was
     * reported. The count only appears once the walk has finished and a
     * total is actually known; before that the honest word is the verb.
     */
    const scanLabel = $derived.by(() => {
        const progress = trackStore.progress;
        if (!progress || progress.total === 0) return "Scanning…";
        return `Scanning ${progress.done} of ${progress.total}…`;
    });

    /**
     * The file being read right now.
     *
     * Shown under the button while a scan runs, so a count that stops
     * moving names what it stopped on. "It froze at 743" is a number; "it
     * froze on this file" is a lead.
     */
    const scanFile = $derived(
        trackStore.progress?.file?.split(/[\/]/).pop() ?? null,
    );
</script>

<PageShell title="Settings" subtitle="Everything here is local to this machine.">
    <div class="mx-auto flex max-w-3xl flex-col gap-4 px-2">
        <!--
          First, above everything, and only when it is true.

          Nothing else on this page matters if the app cannot make a sound, and
          the failure is invisible until a track is clicked: the library lists,
          playlists open, every control responds. Saying it here, before the
          listener goes looking for a setting that would explain it, is the
          whole point.
        -->
        {#if decoder.checked && !decoder.present}
            <div
                class="border-destructive/40 bg-destructive/10 flex items-start gap-3 rounded-xl border px-4 py-3"
                role="alert"
            >
                <TriangleAlertIcon class="text-destructive mt-0.5 size-4 shrink-0" />
                <div class="flex min-w-0 flex-col gap-1">
                    <h2 class="text-destructive text-sm font-semibold">
                        Nothing can play right now
                    </h2>
                    <p class="text-[13px] leading-relaxed">
                        {decoder.message}
                    </p>
                </div>
            </div>
        {/if}

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
                    {trackStore.scanning ? scanLabel : "Rescan"}
                </Button>
                {#if trackStore.scanning && scanFile}
                    <!-- Which file, so a count that stops moving names its
                         own cause instead of leaving it to be guessed. -->
                    <span
                        class="text-muted-foreground max-w-[18rem] truncate text-xs"
                        title={trackStore.progress?.file ?? ""}
                    >
                        {scanFile}
                    </span>
                {/if}
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

                {#if summary.skippedFiles.length > 0}
                    <!--
                      Named, because this is the one verdict the scan makes
                      that could be wrong. A file abandoned for being slow is
                      absent from the library, and without this nothing would
                      say which.
                    -->
                    <div class="mt-2 flex flex-col gap-0.5">
                        <span class="text-muted-foreground text-xs">
                            Gave up reading these — they took too long, which
                            usually means the file is damaged:
                        </span>
                        {#each summary.skippedFiles as path (path)}
                            <span
                                class="text-muted-foreground truncate font-mono text-[11px]"
                                title={path}
                            >
                                {path}
                            </span>
                        {/each}
                    </div>
                {/if}
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

        <LoudnessSettings />
        <EqualizerSettings />
        <GaplessSettings />
        <SilenceSettings />

        <CacheSettings />

        <ExtractorSettings />

        <TagManager />
    </div>
</PageShell>

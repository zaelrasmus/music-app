<script lang="ts">
    import PageShell from "$components/page-shell.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import { Button } from "$components/ui/button";
    import { details, type Source, type TrackProposal } from "$lib/details.svelte";
    import { nav } from "$lib/nav.svelte";
    import ArrowLeftIcon from "@lucide/svelte/icons/arrow-left";
    import FolderIcon from "@lucide/svelte/icons/folder";
    import ChevronRightIcon from "@lucide/svelte/icons/chevron-right";
    import CheckIcon from "@lucide/svelte/icons/check";

    /**
     * Reviewing what the app can work out about untagged tracks.
     *
     * Two depths in one view: the folders, then one folder's tracks. Depth
     * rather than two views because going back has to keep the list exactly
     * where it was — the whole job is working down it.
     */
    const open = $derived(details.open);

    const SOURCES: { id: Source; label: string }[] = [
        { id: "title", label: "Title" },
        { id: "folder", label: "Folder" },
        { id: "skip", label: "Skip" },
    ];

    function line(row: TrackProposal) {
        const chosen = details.resolve(row);
        return chosen ? `${chosen.artist} — ${chosen.title}` : null;
    }
</script>

{#if open}
    <PageShell
        title={open.name}
        badge={open.total}
        subtitle="{details.accepted} of {open.total} will be filled in. Nothing is written until you apply."
    >
        {#snippet leading()}
            <Button
                variant="ghost"
                size="icon"
                aria-label="Back to folders"
                onclick={() => details.close()}
            >
                <ArrowLeftIcon />
            </Button>
        {/snippet}

        {#snippet actions()}
            <Button
                disabled={details.accepted === 0 || details.saving}
                onclick={() => void details.apply()}
            >
                <CheckIcon />
                Apply {details.accepted}
            </Button>
        {/snippet}

        {#snippet toolbar()}
            <div class="flex flex-wrap items-center gap-2 pb-3">
                <span class="text-muted-foreground text-[13px]">Set all to</span>
                {#each SOURCES as source (source.id)}
                    <Button
                        variant="outline"
                        size="sm"
                        onclick={() => details.chooseAll(source.id)}
                    >
                        {source.label}
                    </Button>
                {/each}

                <!--
                  Deliberately its own question. "Is this folder the artist?"
                  and "is it the album?" have different answers -- Celeste is
                  an album and not an artist, Creo is an artist and not an
                  album -- so one switch answering both would be wrong for
                  most folders in this library.
                -->
                <label
                    class="ml-auto flex cursor-pointer items-center gap-2 text-[13px]"
                >
                    <input
                        type="checkbox"
                        class="accent-primary size-4"
                        bind:checked={details.useFolderAsAlbum}
                    />
                    Also set album to “{open.name}”
                </label>
            </div>

            {#if details.needsFolder > 0}
                <p class="text-muted-foreground pb-3 text-[13px] leading-relaxed">
                    {details.needsFolder}
                    {details.needsFolder === 1 ? "track has" : "tracks have"} nothing
                    in the title to go on. Those are only filled in if you choose
                    “Folder”, which is right when “{open.name}” is an artist and
                    wrong when it is a genre.
                </p>
            {/if}
        {/snippet}

        <ul class="flex flex-col gap-1 pb-6">
            {#each details.rows as row (row.trackId)}
                {@const chosen = line(row)}
                <li
                    class="hover:bg-muted/40 flex items-center gap-3 rounded-lg px-3 py-2"
                >
                    <div class="min-w-0 flex-1">
                        {#if chosen}
                            <p class="truncate text-[13px] font-medium">{chosen}</p>
                        {:else}
                            <p
                                class="text-muted-foreground truncate text-[13px] italic"
                            >
                                Left as it is
                            </p>
                        {/if}
                        <!--
                          The current title always shows. This is a list of
                          changes, and a change nobody can see the other half
                          of is not reviewable.
                        -->
                        <p class="text-muted-foreground truncate text-xs">
                            {row.currentTitle}
                        </p>
                    </div>

                    <div class="flex shrink-0 gap-1">
                        {#each SOURCES as source (source.id)}
                            {@const unavailable =
                                source.id === "title" && row.fromTitle === null}
                            <Button
                                variant={details.sourceFor(row) === source.id
                                    ? "default"
                                    : "ghost"}
                                size="sm"
                                class="h-7 px-2 text-xs"
                                disabled={unavailable}
                                title={unavailable
                                    ? "This title has no artist in it"
                                    : undefined}
                                onclick={() => details.choose(row.trackId, source.id)}
                            >
                                {source.label}
                            </Button>
                        {/each}
                    </div>
                </li>
            {/each}
        </ul>
    </PageShell>
{:else}
    <PageShell
        title="Fill in missing details"
        badge={details.folders.reduce((n, f) => n + f.total, 0) || null}
        subtitle="Tracks whose files carry no artist tag, grouped by the folder they sit in."
    >
        {#snippet leading()}
            <Button
                variant="ghost"
                size="icon"
                aria-label="Back to settings"
                onclick={() => nav.go("settings")}
            >
                <ArrowLeftIcon />
            </Button>
        {/snippet}

        {#if details.loading && details.folders.length === 0}
            <p class="text-muted-foreground px-3 py-6 text-sm">Looking…</p>
        {:else if details.folders.length === 0}
            <EmptyState
                icon={CheckIcon}
                title="Every track has an artist"
                hint="Nothing here needs filling in."
            />
        {:else}
            <ul class="flex flex-col gap-1 pb-6">
                {#each details.folders as folder (folder.path)}
                    <li>
                        <button
                            type="button"
                            class="hover:bg-muted/40 flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left"
                            onclick={() => void details.openFolder(folder)}
                        >
                            <FolderIcon
                                class="text-muted-foreground size-4 shrink-0"
                            />
                            <div class="min-w-0 flex-1">
                                <p class="truncate text-[13px] font-medium">
                                    {folder.name}
                                </p>
                                <!--
                                  The second number is the one that decides
                                  whether this folder is quick: it says how
                                  many the filenames answer for without any
                                  judgement being needed.
                                -->
                                <p class="text-muted-foreground truncate text-xs">
                                    {folder.total}
                                    {folder.total === 1 ? "track" : "tracks"} ·
                                    {folder.fromTitles} from their titles
                                </p>
                            </div>
                            <ChevronRightIcon
                                class="text-muted-foreground size-4 shrink-0"
                            />
                        </button>
                    </li>
                {/each}
            </ul>
        {/if}
    </PageShell>
{/if}

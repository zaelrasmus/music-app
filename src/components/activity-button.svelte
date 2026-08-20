<script lang="ts">
    import { downloads } from "$lib/downloads.svelte";
    import DownloadIcon from "@lucide/svelte/icons/download";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import CheckIcon from "@lucide/svelte/icons/check";
    import AlertIcon from "@lucide/svelte/icons/triangle-alert";
    import ChevronRightIcon from "@lucide/svelte/icons/chevron-right";
    import XIcon from "@lucide/svelte/icons/x";

    let open = $state(false);

    const pending = $derived(downloads.pending);
    const failed = $derived(downloads.failed);
    const busy = $derived(downloads.busy);

    /**
     * Nothing to show and nothing to say.
     *
     * The button disappears entirely when the app is idle and has no history
     * worth keeping. A permanent control that is empty nine tenths of the time
     * teaches people to ignore it, which is exactly the wrong lesson for the
     * one time it is trying to report a failure.
     */
    const shown = $derived(busy || downloads.jobs.length > 0);

    const label = $derived(
        pending > 0
            ? `${pending} downloading`
            : failed > 0
              ? `${failed} failed`
              : "Downloads",
    );

    function close() {
        open = false;
    }
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") close();
    }}
/>

{#if shown}
    <div class="relative">
        <button
            type="button"
            class="text-titlebar-foreground hover:bg-foreground/10 hover:text-foreground focus-visible:bg-foreground/10 relative inline-grid size-7 shrink-0 place-items-center rounded-md transition-colors focus-visible:outline-none
                   {open ? 'bg-foreground/10 text-foreground' : ''}"
            aria-label={label}
            title={label}
            aria-expanded={open}
            onclick={() => (open = !open)}
        >
            {#if pending > 0}
                <LoaderIcon class="size-[15px] animate-spin" />
            {:else if failed > 0}
                <AlertIcon class="size-[15px]" />
            {:else}
                <DownloadIcon class="size-[15px]" />
            {/if}

            <!--
              The count, not a dot. "Three waiting" is a different situation
              from "one waiting", and the queue runs one at a time — so the
              number is the only thing that says how long this will take.
            -->
            {#if pending > 1}
                <span
                    class="bg-primary text-primary-foreground absolute -right-0.5 -bottom-0.5 grid min-w-3.5 place-items-center rounded-full px-[3px] text-[9px] leading-[14px] font-semibold tabular-nums"
                >
                    {pending}
                </span>
            {/if}
        </button>

        {#if open}
            <!-- Click-away. Transparent, behind the panel, above everything
                 else — the ordinary way to dismiss a popover without trapping
                 focus or blocking the window controls. -->
            <button
                type="button"
                class="fixed inset-0 z-40 cursor-default"
                aria-label="Close downloads"
                onclick={close}
            ></button>

            <div
                class="bg-popover text-popover-foreground absolute right-0 z-50 mt-1 flex max-h-[26rem] w-[22rem] flex-col overflow-hidden rounded-lg border shadow-lg"
            >
                <header
                    class="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-2"
                >
                    <h2 class="text-xs font-semibold">Downloads</h2>
                    {#if downloads.jobs.some((j) => j.state !== "queued" && j.state !== "running")}
                        <button
                            type="button"
                            class="text-muted-foreground hover:text-foreground text-[11px] underline underline-offset-2 transition-colors"
                            onclick={() => downloads.clearFinished()}
                        >
                            Clear finished
                        </button>
                    {/if}
                </header>

                <div class="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-2">
                    {#each downloads.grouped as batch (batch.group.id)}
                        {@const isOpen = downloads.expanded.has(batch.group.id)}
                        <!--
                          A batch is one line until it is asked to be more.
                          Fifty queued tracks under a playlist name is a list
                          nobody reads; "Discovery — 12 of 33" is the same
                          information in one glance.
                        -->
                        <button
                            type="button"
                            class="hover:bg-accent/60 flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors"
                            aria-expanded={isOpen}
                            onclick={() => downloads.toggleGroup(batch.group.id)}
                        >
                            <ChevronRightIcon
                                class="text-muted-foreground size-3.5 shrink-0 transition-transform {isOpen
                                    ? 'rotate-90'
                                    : ''}"
                            />
                            <span class="min-w-0 flex-1 truncate text-xs font-medium">
                                {batch.group.name}
                            </span>
                            <span
                                class="text-muted-foreground shrink-0 text-[11px] tabular-nums"
                            >
                                {batch.done} of {batch.jobs.length}
                                {#if batch.failed > 0}
                                    · {batch.failed} failed
                                {/if}
                            </span>
                        </button>

                        {#if isOpen}
                            <ul class="mb-1 flex flex-col gap-px pl-5">
                                {#each batch.jobs as job (job.id)}
                                    {@render jobRow(job)}
                                {/each}
                            </ul>
                        {/if}
                    {/each}

                    {#each downloads.loose as job (job.id)}
                        {@render jobRow(job)}
                    {/each}

                    {#if downloads.jobs.length === 0}
                        <p class="text-muted-foreground px-2 py-3 text-xs">
                            Nothing downloading.
                        </p>
                    {/if}

                    <!--
                      Caching, last and quiet.

                      This is work the user allowed rather than asked for — the
                      rest of a track they walked away from, fetched so it
                      plays offline later. It earns a line, not a row.
                    -->
                    {#if downloads.caching.length > 0}
                        <div class="mt-1 border-t pt-2">
                            {#each downloads.caching as item (item.trackId)}
                                <p
                                    class="text-muted-foreground truncate px-2 py-0.5 text-[11px]"
                                    title="Kept for offline play because you left this track part-way through"
                                >
                                    Caching {item.title}
                                </p>
                            {/each}
                        </div>
                    {/if}
                </div>
            </div>
        {/if}
    </div>
{/if}

{#snippet jobRow(job: import("$lib/downloads.svelte").Job)}
    <li
        class="group/job hover:bg-accent/40 flex items-center gap-2 rounded-md px-2 py-1 transition-colors"
    >
        <span class="grid size-3.5 shrink-0 place-items-center">
            {#if job.state === "running"}
                <LoaderIcon class="text-primary size-3.5 animate-spin" />
            {:else if job.state === "done"}
                <CheckIcon class="text-signal size-3.5" />
            {:else if job.state === "failed"}
                <AlertIcon class="text-destructive size-3.5" />
            {:else}
                <span class="bg-muted-foreground/40 size-1.5 rounded-full"></span>
            {/if}
        </span>

        <span class="flex min-w-0 flex-1 flex-col">
            <span class="truncate text-[11px] leading-tight">{job.title}</span>
            {#if job.error}
                <span class="text-destructive truncate text-[10px] leading-tight">
                    {job.error}
                </span>
            {:else if job.artist}
                <span class="text-muted-foreground truncate text-[10px] leading-tight">
                    {job.artist}
                </span>
            {/if}
        </span>

        <!-- Only a job that has not started can be dropped; stopping one
             mid-write is a larger promise than this button can make. -->
        {#if job.state === "queued"}
            <button
                type="button"
                class="text-muted-foreground hover:bg-background hover:text-foreground hidden size-5 shrink-0 place-items-center rounded transition-colors group-hover/job:grid"
                aria-label="Remove {job.title} from the download queue"
                onclick={() => downloads.cancel(job.id)}
            >
                <XIcon class="size-3" />
            </button>
        {/if}
    </li>
{/snippet}

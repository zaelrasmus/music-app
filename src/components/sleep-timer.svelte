<script lang="ts">
    import { Popover } from "bits-ui";
    import MoonIcon from "@lucide/svelte/icons/moon";
    import { sleepStore, SLEEP_MINUTES, formatCountdown } from "$lib/sleep.svelte";

    let open = $state(false);

    const ghost =
        "relative grid size-8 shrink-0 place-items-center rounded-md transition-colors hover:bg-accent focus-visible:outline-none focus-visible:bg-accent";

    async function choose(action: () => Promise<void>) {
        await action();
        open = false;
    }

    const label = $derived(
        sleepStore.endOfTrack
            ? "Stopping at the end of this track"
            : sleepStore.remaining !== null
              ? `Stopping in ${formatCountdown(sleepStore.remaining)}`
              : "Sleep timer",
    );
</script>

<Popover.Root bind:open>
    <Popover.Trigger
        class="{ghost} {sleepStore.armed ? 'text-foreground' : 'text-muted-foreground hover:text-foreground'}"
        aria-label={label}
        title={label}
    >
        <MoonIcon class="size-4" />
        <!--
          The countdown lives on the button, not only inside the popover.

          A timer you have to open a menu to see is one you will wonder about,
          and the entire feeling this feature sells is not having to wonder
          whether the music will stop.
        -->
        {#if sleepStore.armed}
            <span
                class="bg-primary text-primary-foreground absolute -top-0.5 -right-1 rounded-full px-1 text-[9px] leading-[14px] font-medium tabular-nums"
            >
                {sleepStore.endOfTrack
                    ? "end"
                    : formatCountdown(sleepStore.remaining ?? 0)}
            </span>
        {/if}
    </Popover.Trigger>

    <Popover.Portal>
        <Popover.Content
            side="top"
            sideOffset={8}
            align="end"
            class="bg-popover text-popover-foreground border-border z-50 w-52 rounded-lg border p-1 shadow-lg outline-none"
        >
            <p class="text-muted-foreground px-2 py-1.5 text-[11px]">
                Pause playback after…
            </p>

            {#each SLEEP_MINUTES as minutes (minutes)}
                <button
                    type="button"
                    class="hover:bg-accent flex w-full items-center rounded-md px-2 py-1.5 text-left text-[13px] transition-colors"
                    onclick={() => choose(() => sleepStore.setMinutes(minutes))}
                >
                    {minutes} minutes
                </button>
            {/each}

            <!--
              Its own option because it is a different promise: a track can be
              paused, seeked or skipped, so "however long is left" computed once
              would stop the music in the middle of something.
            -->
            <button
                type="button"
                class="hover:bg-accent flex w-full items-center rounded-md px-2 py-1.5 text-left text-[13px] transition-colors"
                onclick={() => choose(() => sleepStore.setEndOfTrack())}
            >
                End of this track
            </button>

            {#if sleepStore.armed}
                <div class="bg-border my-1 h-px"></div>
                <button
                    type="button"
                    class="hover:bg-accent text-muted-foreground hover:text-foreground flex w-full items-center rounded-md px-2 py-1.5 text-left text-[13px] transition-colors"
                    onclick={() => choose(() => sleepStore.cancel())}
                >
                    Cancel the timer
                </button>
            {/if}
        </Popover.Content>
    </Popover.Portal>
</Popover.Root>

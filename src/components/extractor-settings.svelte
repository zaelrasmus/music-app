<script lang="ts">
    import { Button } from "$components/ui/button";
    import SettingsSection from "$components/settings-section.svelte";
    import { ytDlp } from "$lib/ytdlp.svelte";
    import DownloadIcon from "@lucide/svelte/icons/download";
    import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";

    /**
     * Days, not a timestamp: the useful question is "is this stale", and a
     * date makes the reader do that subtraction themselves.
     */
    function describeCheck(checkedAt: number | null) {
        if (checkedAt === null) return "Not checked yet";

        const days = Math.floor((Date.now() / 1000 - checkedAt) / 86400);
        if (days <= 0) return "Checked today";
        if (days === 1) return "Checked yesterday";
        return `Checked ${days} days ago`;
    }

    const checked = $derived(describeCheck(ytDlp.checkedAt));
</script>

<SettingsSection
    icon={DownloadIcon}
    title="Streaming extractor"
    description="yt-dlp is what finds the audio behind a YouTube or SoundCloud page. YouTube changes what it serves from time to time, which is what makes tracks stop playing; the app follows the nightly builds and updates itself when that happens."
>
    {#snippet actions()}
        <Button
            variant="outline"
            size="sm"
            disabled={ytDlp.updating}
            onclick={() => ytDlp.check()}
        >
            <RefreshCwIcon
                data-icon="inline-start"
                class={ytDlp.updating ? "animate-spin" : ""}
            />
            {ytDlp.updating ? "Checking…" : "Check for updates"}
        </Button>
    {/snippet}

    <div class="flex flex-col gap-2">
        <div class="text-muted-foreground flex items-baseline justify-between text-xs">
            <span class="selectable tabular-nums">
                {ytDlp.version ?? "Version unknown"}
                <span class="opacity-70">· {ytDlp.channel}</span>
            </span>
            <span class="tabular-nums">{checked}</span>
        </div>

        {#if ytDlp.error}
            <p
                class="border-destructive/50 bg-destructive/5 text-destructive selectable rounded-md border px-3 py-2 text-[13px]"
                role="alert"
            >
                {ytDlp.error}
            </p>
        {/if}
    </div>
</SettingsSection>

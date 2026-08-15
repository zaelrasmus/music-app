<script lang="ts">
    /**
     * A draggable bar, used for both seeking and volume.
     *
     * Hand-rolled rather than the shadcn slider because both uses want the same
     * two behaviours that a generic slider does not give: a *live* value during
     * the drag that is distinct from the committed one (seeking must not fire a
     * hundred times on the way past), and a track that is 4px at rest and
     * thickens under the pointer, so the bar reads as a progress indicator
     * first and a control second.
     */
    interface Props {
        value: number;
        max: number;
        /** Fired continuously while dragging. Cheap operations only. */
        onScrub: (value: number) => void;
        /** Fired once, on release. This is the one that costs something. */
        onCommit?: (value: number) => void;
        disabled?: boolean;
        label: string;
        /** Larger keyboard steps for a seek bar than for volume. */
        step?: number;
        /** Announced instead of the raw number, e.g. "1:23 of 4:05". */
        valueText?: string;
        class?: string;
    }

    let {
        value,
        max,
        onScrub,
        onCommit,
        disabled = false,
        label,
        step = 1,
        valueText,
        class: className = "",
    }: Props = $props();

    let track = $state<HTMLDivElement | null>(null);
    let dragging = $state(false);
    let hovering = $state(false);

    const fraction = $derived(max <= 0 ? 0 : Math.min(Math.max(value / max, 0), 1));
    /** The thumb and the thicker track share one trigger. */
    const active = $derived(!disabled && (hovering || dragging));

    function valueAt(clientX: number) {
        if (!track) return 0;
        const rect = track.getBoundingClientRect();
        if (rect.width === 0) return 0;
        const ratio = (clientX - rect.left) / rect.width;
        return Math.min(Math.max(ratio, 0), 1) * max;
    }

    function down(event: PointerEvent) {
        if (disabled) return;
        track?.setPointerCapture(event.pointerId);
        dragging = true;
        onScrub(valueAt(event.clientX));
        event.preventDefault();
    }

    function move(event: PointerEvent) {
        if (!dragging) return;
        onScrub(valueAt(event.clientX));
    }

    function up(event: PointerEvent) {
        if (!dragging) return;
        track?.releasePointerCapture(event.pointerId);
        dragging = false;
        // Committing from the event rather than from `value` avoids landing on
        // a stale prop if the parent has not re-rendered since the last move.
        (onCommit ?? onScrub)(valueAt(event.clientX));
    }

    function key(event: KeyboardEvent) {
        if (disabled) return;

        const big = event.shiftKey ? 4 : 1;
        let next: number | null = null;

        if (event.key === "ArrowLeft" || event.key === "ArrowDown") {
            next = value - step * big;
        } else if (event.key === "ArrowRight" || event.key === "ArrowUp") {
            next = value + step * big;
        } else if (event.key === "Home") {
            next = 0;
        } else if (event.key === "End") {
            next = max;
        }

        if (next === null) return;
        event.preventDefault();

        const clamped = Math.min(Math.max(next, 0), max);
        onScrub(clamped);
        (onCommit ?? onScrub)(clamped);
    }
</script>

<div
    bind:this={track}
    role="slider"
    tabindex={disabled ? -1 : 0}
    aria-label={label}
    aria-valuemin={0}
    aria-valuemax={max}
    aria-valuenow={value}
    aria-valuetext={valueText}
    aria-disabled={disabled}
    class="group/scrub relative flex h-4 w-full cursor-pointer touch-none items-center focus-visible:outline-none {disabled
        ? 'cursor-default opacity-50'
        : ''} {className}"
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
    onpointerenter={() => (hovering = true)}
    onpointerleave={() => (hovering = false)}
    onkeydown={key}
>
    <div
        class="bg-muted relative w-full overflow-hidden rounded-full transition-[height] duration-100 {active
            ? 'h-1.5'
            : 'h-1'}"
    >
        <div
            class="h-full rounded-full {active ? 'bg-primary' : 'bg-foreground/55'} {dragging
                ? ''
                : 'transition-[width,background-color] duration-100'}"
            style="width: {fraction * 100}%"
        ></div>
    </div>

    <!-- The handle only exists while it is useful. A permanent dot on a 1px
         track is the single easiest way to make a player look cluttered. -->
    <div
        class="bg-foreground pointer-events-none absolute size-3 rounded-full shadow-sm transition-opacity duration-100 {active
            ? 'opacity-100'
            : 'opacity-0'} group-focus-visible/scrub:opacity-100"
        style="left: calc({fraction * 100}% - 6px)"
    ></div>
</div>

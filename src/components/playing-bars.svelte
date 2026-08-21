<script lang="ts">
    import { cn } from "$lib/utils";

    interface Props {
        /** Paused keeps the bars, frozen — the row is still the current one. */
        animate?: boolean;
        class?: string;
    }

    let { animate = true, class: className = "" }: Props = $props();
</script>

<!--
  The "this row is playing" mark.

  A pause icon would be wrong here: the row is not a button in this state, and
  a static glyph gives no sense of whether audio is actually moving. Three bars
  do, and they read at 12px where an icon would not.

  Frozen rather than removed when paused, because the row is still the current
  track — losing the mark entirely would make a paused player look stopped.
-->
<span
    class={cn("flex h-3 items-end gap-[2px]", className)}
    aria-hidden="true"
    data-animate={animate}
>
    <i style="--delay: 0ms; --peak: 100%"></i>
    <i style="--delay: -160ms; --peak: 60%"></i>
    <i style="--delay: -320ms; --peak: 85%"></i>
</span>

<style>
    i {
        display: block;
        width: 2px;
        height: 35%;
        border-radius: 999px;
        background-color: currentColor;
        transform-origin: bottom;
    }

    [data-animate="true"] i {
        animation: bounce 900ms ease-in-out infinite alternate;
        animation-delay: var(--delay);
    }

    @keyframes bounce {
        from {
            height: 25%;
        }
        to {
            height: var(--peak);
        }
    }
</style>

<script lang="ts">
	import "./layout.css";
	import { onMount } from "svelte";
	import { ModeWatcher } from "mode-watcher";
	import { Toaster } from "$components/ui/sonner/index.js";
	import Titlebar from "$components/titlebar.svelte";
	import AppSidebar from "$components/app-sidebar.svelte";
	import PlayerBar from "$components/player-bar.svelte";
	import QueuePanel from "$components/queue-panel.svelte";
	import LyricsView from "$components/lyrics-view.svelte";
	import PromptDialog from "$components/prompt-dialog.svelte";
	import MetadataDialog from "$components/metadata-dialog.svelte";
	import { chrome } from "$lib/chrome.svelte";
	import { sidebar } from "$lib/sidebar.svelte";
	import { queueStore } from "$lib/queue.svelte";
	import { player } from "$lib/player.svelte";
	import { lyricsStore } from "$lib/lyrics.svelte";
	import { selection } from "$lib/selection.svelte";

	const { children } = $props();

	/**
	 * Lyrics belong to a track, so changing tracks invalidates them.
	 *
	 * Here rather than inside the panel because the panel is only mounted
	 * while it is open: without this, opening it after three skips would show
	 * whatever was found for the track playing when it was last closed.
	 */
	$effect(() => {
		lyricsStore.trackChanged(player.trackId);
	});


	/**
	 * The window is configured `visible: false`, so showing it is this app's
	 * job. Done here rather than in a view because the layout is the last thing
	 * that can fail before something is on screen -- and a window that never
	 * appears is indistinguishable from an app that never started.
	 */
	onMount(() => {
		const chromeReady = chrome.start();
		void sidebar.restore();

		return () => {
			chromeReady.then((stop) => stop());
		};
	});

	/**
	 * Shortcuts.
	 *
	 * Deliberately few, and none of them single letters: this app has text
	 * inputs on every view, and a bare `s` for search would fight every one of
	 * them. Anything typed into a field is left alone.
	 */
	function shortcut(event: KeyboardEvent) {
		// Before the typing guard: Escape is how you get out of a selection, and
		// it has to work whatever has focus.
		//
		// Lyrics come first because they cover the list a selection was made
		// in — closing the thing you can actually see is what Escape means.
		if (event.key === "Escape" && lyricsStore.open) {
			lyricsStore.close();
			return;
		}
		if (event.key === "Escape" && selection.active) {
			selection.clear();
			return;
		}

		const target = event.target as HTMLElement | null;
		const typing =
			target?.tagName === "INPUT" ||
			target?.tagName === "TEXTAREA" ||
			target?.isContentEditable === true;

		if (event.ctrlKey && !event.shiftKey && !event.altKey && event.key === "b") {
			event.preventDefault();
			sidebar.cycle();
			return;
		}

		if (event.ctrlKey && !event.shiftKey && !event.altKey && event.key === "q") {
			event.preventDefault();
			queueStore.toggle();
			return;
		}

		if (event.ctrlKey && !event.shiftKey && !event.altKey && event.key === "l") {
			event.preventDefault();
			lyricsStore.toggle();
			return;
		}

		if (typing) return;

		// Nudging the lyrics against the audio. Unmodified, because it is a
		// tuning gesture repeated until it looks right, and a modifier makes
		// that tedious — but only while the panel is open, so the keys are
		// free everywhere else.
		if (lyricsStore.open && (event.key === "[" || event.key === "]")) {
			event.preventDefault();
			void lyricsStore.nudge(event.key === "[" ? 1 : -1);
			return;
		}

		// Space is the one unmodified key worth taking. It is what every player
		// uses, and the check above means it still types a space in a search box.
		if (event.key === " ") {
			event.preventDefault();
			void player.togglePlayPause();
		}
	}
</script>

<svelte:window onkeydown={shortcut} />

<!-- `track` keeps following the OS until the user pins a mode; the dark class
     goes on <html>, which is what `@custom-variant dark` matches. -->
<ModeWatcher />

<Toaster position="bottom-center" />
<PromptDialog />
<MetadataDialog />

<!--
  The shell.

  `h-screen` with every region a flex child, so nothing is positioned
  absolutely and nothing needs bottom padding to clear the player bar. The
  window itself never scrolls -- the one scroll container is inside whichever
  view is open.
-->
<div class="flex h-screen flex-col overflow-hidden">
	<Titlebar />

	<div class="flex min-h-0 flex-1 overflow-hidden">
		<AppSidebar />

		<!-- `relative` so the lyrics panel can cover the view without covering
		     the sidebar or the player bar, both of which stay usable. -->
		<main class="relative min-w-0 flex-1 overflow-hidden">
			{@render children()}
			{#if lyricsStore.open}
				<LyricsView />
			{/if}
		</main>

		<QueuePanel />
	</div>

	<PlayerBar />
</div>

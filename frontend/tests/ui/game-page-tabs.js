import { mount } from 'svelte';
import '../../src/app.css';
import GamePageTabs from './GamePageTabs.svelte';

mount(GamePageTabs, { target: document.getElementById('app') });

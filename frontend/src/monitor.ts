import { isTauri } from '@tauri-apps/api/core';
import { mount } from 'svelte';
import Monitor from './components/Monitor.svelte';

if (isTauri()) mount(Monitor, { target: document.getElementById('app')! });

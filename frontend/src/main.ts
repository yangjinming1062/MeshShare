import App from './App.svelte';
import './app.css';
import { mount } from 'svelte';

if (!document.documentElement.hasAttribute('data-update-webview')) {
  mount(App, { target: document.getElementById('app')! });
}

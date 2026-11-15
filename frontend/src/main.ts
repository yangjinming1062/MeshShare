import 'core-js/es/string/replace-all';
import App from './App.svelte';
import './app.css';
import { mount } from 'svelte';

const app = mount(App, { target: document.getElementById('app')! });

export default app;

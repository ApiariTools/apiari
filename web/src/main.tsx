import { createRoot } from 'react-dom/client'
import './index.css'
import './theme.css'
import App from './App'

// iOS Safari: enable click event bubbling on non-interactive elements
document.getElementById('root')!.addEventListener('click', () => {});
// iOS Safari: enable :active pseudo-class
document.addEventListener('touchstart', () => {}, { passive: true });

createRoot(document.getElementById('root')!).render(<App />)

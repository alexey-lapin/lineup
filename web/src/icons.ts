import { addCollection } from '@iconify/vue'
import icons from './icons.json'

// Register the bundled subset so no icon is fetched from the Iconify API at runtime.
addCollection(icons)

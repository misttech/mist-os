# Adding diagrams to documentation

Diagrams are useful in documentation to help illustrate engineering concepts and
guides. However, it's important to ensure diagrams on fuchia.dev are accessible
and updatable. This guide shows you how to add maintainable diagrams to Fuchsia
documentation.

## Types of diagrams

* **Option 1:** Generate a simple flow diagram from text in the source code
(example: [Sequence Diagram](https://bramp.github.io/js-sequence-diagrams/) or
go/sequencediagram for Googlers)

* **Option 2:** Create a visual diagram in Google Drawings, Slides, or other tool

{% dynamic if user.is_googler %}

* **Option 3:** For more complex images, high-level concepts, or diagrams that
require UX design, partner with XD to create a diagram in Figma. [File a bug](https://buganizer.corp.google.com/issues/new?component=938361&template=1648923)
to request assistance.

{% dynamic endif %}

## How to add visual diagrams

If you are creating a visual diagram for documentation on fuchsia.dev, follow
these steps:

**Step 1:** Create a visual diagram in your tool of choice and export the image
as a .svg (suggested) or .png file
* Optimize your image to ensure it loads quickly. Large images can slow down
page load times.
* Use SVG (Scalable Vector Graphics) format whenever possible. SVGs are
scalable, accessible, and have a smaller file size compared to other image
formats.

**Step 2:** Embed the image on fuchsia.dev using Markdown [see instructions](/docs/contribute/docs/markdown.md#images)
* Store your image in the same directory as your Markdown file

**Step 3:** Add alt text to the image. Alt text helps screen reader users
understand the content of your image.

```
![Alt text](image_filename.svg)
```

* Replace `Alt text` with a concise description of the image
* Replace `image_filename.svg` with the actual filename of your image

Example:

```
![A flowchart showing the boot process of a Fuchsia device. The process starts
with the bootloader, then moves to Zircon kernel, and finally to the user
space.](component_framework.svg)
```

{% dynamic if user.is_googler %}

**Step 4:** Create an internal Buganizer bug [using this template](https://buganizer.corp.google.com/issues/new?component=1702186&template=2076662).
Include:
* Author or owner - best contact for maintaining the diagram
* Link to source file (Google drawings, Slides, Figma, etc - ensure files are
shared with rvos)
* Link to page on fuchsia.dev where diagram can be found
* Alt text

**Step 5:** Add a comment in Markdown with a link to the bug that contains the
diagram source information

```
{# Diagram source: link to bug #}
```

Note: We are intentionally making these bugs internal-only, even though there
will be some instances when the diagram is visible to the public on an open-
source page. We acknowledge this is not best practice but the bug links will be
labeled “Diagram source” for transparency.

{% dynamic endif %}


## UX guidelines for diagrams

Diagrams are a useful tool for providing additional clarity or context that is
not easily conveyed through words alone. Effective diagrams should directly
support and enhance the accompanying text in documentation.

**Focus:** Each diagram should have a clear focus and convey a specific concept
or idea. Label all elements clearly and concisely.

**Simplicity:** Strive for visual clarity. Avoid excessive complexity or visual
clutter that might confuse the reader.

**Consistency:** Maintain a consistent style and visual language across all
diagrams within Fuchsia documentation.

**Internationalization:** Keep text within diagrams to a minimum. If text is
necessary, ensure it is translatable and localized. Use universally recognized
symbols and icons whenever possible.

### Design considerations

Visual hierarchy: Use clear and consistent visual hierarchy to guide the
reader's eye through the diagram. Use whitespace to create visual separation and
improve readability.

**Color:** Use sufficient color contrast to ensure readability for users with
visual impairments. Avoid using color as the sole means of conveying
information.

**Scalability:** Ensure diagrams are scalable and can be zoomed in without
losing clarity.

### Alt text

Provide concise and informative alternative text descriptions for all diagrams.

Alt text should:

* Accurately describe the content and purpose of the diagram
* Use plain language that is easy for everyone to understand
* Be concise and to the point, avoiding unnecessary detail
* If a diagram contains text, include that text in the alt text

Note: Avoid using phrases like "image of" or "diagram of" as screen readers
already identify them as images.

**Alt text examples**

<table>
  <tr>
   <td>A flowchart showing the boot process of a Fuchsia device. The process
   starts with the bootloader, then moves to Zircon kernel, and finally to the
   user space.</td>
   <td>Image of a flowchart. (Too generic and not informative)</td>
  </tr>
  <tr>
   <td style="background-color: #d9ead3">Do
   </td>
   <td style="background-color: #f4cccc">Don’t
   </td>
  </tr>
</table>

<table>
  <tr>
   <td>A diagram illustrating the relationship between components in a Fuchsia
   component framework. The components are arranged hierarchically, with parent
   components at the top and child components at the bottom.</td>
   <td>Diagram with boxes and arrows. (Not descriptive enough)</td>
  </tr>
  <tr>
   <td style="background-color: #d9ead3">Do
   </td>
   <td style="background-color: #f4cccc">Don’t
   </td>
  </tr>
</table>
# \<e2e_test/templates.h\> in e2e

[Header source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h)

## BaseTemplate class {:#BaseTemplate}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#13)

<pre class="devsite-disable-click-to-copy">
<span class="kwd">template</span>&lt;<span class="kwd">typename</span><span class="typ"> T</span>, <span class="typ">int i = 1</span>&gt;
<span class="kwd">class</span> BaseTemplate { <span class="com">...</span> };
</pre>

A base template type.

### BaseTemplate::GetValue() {:#BaseTemplate::GetValue}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#15)

<pre class="devsite-disable-click-to-copy">
<span class="typ">T</span> BaseTemplate::<b>GetValue</b>();
</pre>


## BaseTemplate&lt;T, 0&gt; class specialization {:#BaseTemplate}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#19)

<pre class="devsite-disable-click-to-copy">
<span class="kwd">template</span>&lt;<span class="kwd">typename</span><span class="typ"> T</span>&gt;
<span class="kwd">class</span> BaseTemplate&lt;<span class="typ">T</span>, <span class="typ">0</span>&gt; { <span class="com">...</span> };
</pre>

Partial template specialization.

## BaseTemplate&lt;int, 0&gt; class specialization {:#BaseTemplate}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#23)

<pre class="devsite-disable-click-to-copy">
<span class="kwd">template</span>&lt;&gt;
<span class="kwd">class</span> BaseTemplate&lt;<span class="typ">int</span>, <span class="typ">0</span>&gt; { <span class="com">...</span> };
</pre>

Full template specialization.

### BaseTemplate&lt;int, 0&gt;::TemplateFunctionOnBase() {:#BaseTemplate::TemplateFunctionOnBase}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#26)

<pre class="devsite-disable-click-to-copy">
<span class="typ">int</span> BaseTemplate&lt;<span class="typ">int</span>, <span class="typ">0</span>&gt;::<b>TemplateFunctionOnBase</b>();
</pre>


## DerivesFromTemplate class {:#DerivesFromTemplate}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#30)

<pre class="devsite-disable-click-to-copy">
<span class="kwd">class</span> DerivesFromTemplate : <span class="kwd">public</span> <span class="typ"><a href="templates.h.md#BaseTemplate">GlobalNamespace::BaseTemplate&lt;int, 0&gt;</a></span> { <span class="com">...</span> };
</pre>

Class derived from a template.

### Inherited from [BaseTemplate](templates.h.md#BaseTemplate)

<pre class="devsite-disable-click-to-copy">
<span class="typ">int</span> <a href="templates.h.md#BaseTemplate::TemplateFunctionOnBase"><b>TemplateFunctionOnBase</b></a>();
</pre>

## TemplateFunction(…) {:#TemplateFunction}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#39)

<pre class="devsite-disable-click-to-copy">
<span class="kwd">template</span>&lt;<span class="kwd">class</span><span class="typ"> T</span>, <span class="typ">int s</span>&gt;
<span class="typ">void</span> <b>TemplateFunction</b>(<span class="typ">T</span> t);
</pre>


## TemplateFunction&lt;double, 0&gt;(…) specialization {:#TemplateFunction}

[Declaration source code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/tools/cppdocgen/e2e_test/templates.h#42)

<pre class="devsite-disable-click-to-copy">
<span class="kwd">template</span>&lt;&gt;
<span class="typ">void</span> <b>TemplateFunction</b>&lt;<span class="typ">double</span>, <span class="typ">0</span>&gt;(<span class="typ">double</span>);
</pre>

Templatized function specialization.



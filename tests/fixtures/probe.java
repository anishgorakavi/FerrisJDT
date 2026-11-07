// Test fixture: one file exercising a wide spread of Java 8 syntax, used while
// developing against the tree-sitter grammar. Not a compilable program.
package ds;

import java.util.ArrayList;
import java.util.List;
import java.util.*;

public class Probe<E extends Comparable<? super E>> extends Base implements Iterable<E> {
    private static final int CAP = 10;
    private Node<E> root;
    private List<E> items = new ArrayList<>();
    int[] counts = new int[CAP];

    public Probe() { this(0); }
    public Probe(int n) { super(n); }

    public boolean add(E value) {
        E other = items.get(0);
        int c = value.compareTo(other);
        for (int i = 0; i < counts.length; i++) {
            counts[i] += 1;
        }
        for (E item : items) { System.out.println(item); }
        Runnable r = () -> System.out.println("x");
        Comparator<E> cmp = (a, b) -> a.compareTo(b);
        java.util.function.Function<E, String> f = E::toString;
        if (root != null) { root.left = null; }
        try { throw new IllegalStateException("x"); }
        catch (RuntimeException | Error e) { e.printStackTrace(); }
        finally { }
        switch (c) { case 0: break; default: break; }
        return c > 0 ? true : false;
    }

    static class Node<T> { T data; Node<T> left, right; }
    interface Visitor { void visit(); }
    enum Color { RED, GREEN; }
    @Override public java.util.Iterator<E> iterator() { return items.iterator(); }
}
